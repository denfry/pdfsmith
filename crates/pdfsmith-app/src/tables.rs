//! PDF → Excel без Office: строки и колонки восстанавливаются по геометрии
//! текста (как «потоковый» режим Tabula), книга .xlsx пишется напрямую.
//!
//! Строка — символы с общей серединой по высоте. Внутри строки большой разрыв
//! (больше кегля) делит её на сегменты-ячейки. Подряд идущие строки из нескольких
//! сегментов — таблица: её колонки — объединение перекрывающихся по X сегментов.
//! Остальные строки ложатся текстом в столбец A.

use std::io::Write;
use std::path::Path;

use pdfsmith_pdfium::TextChar;

/// Ячейка листа.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Empty,
    Text(String),
    Number(f64),
}

impl Cell {
    fn from_text(s: String) -> Cell {
        match parse_number(&s) {
            Some(n) => Cell::Number(n),
            None if s.is_empty() => Cell::Empty,
            None => Cell::Text(s),
        }
    }
}

#[derive(Debug)]
struct Segment {
    text: String,
    x0: f32,
    x1: f32,
}

#[derive(Debug)]
struct Line {
    segs: Vec<Segment>,
}

/// Строки листа для одной страницы.
pub fn page_rows(chars: &[TextChar]) -> Vec<Vec<Cell>> {
    let lines = group_lines(chars);
    let mut rows = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].segs.len() < 2 {
            let text = lines[i].segs.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
            rows.push(vec![Cell::from_text(text)]);
            i += 1;
            continue;
        }
        // Таблица: подряд идущие строки из нескольких сегментов.
        let start = i;
        while i < lines.len() && lines[i].segs.len() >= 2 {
            i += 1;
        }
        rows.extend(table_rows(&lines[start..i]));
    }
    rows
}

fn group_lines(chars: &[TextChar]) -> Vec<Line> {
    let mut sorted: Vec<&TextChar> = chars.iter().collect();
    // Сверху вниз (в PDF ось Y вверх), затем слева направо.
    sorted.sort_by(|a, b| mid(b).total_cmp(&mid(a)).then(a.left.total_cmp(&b.left)));
    let mut groups: Vec<(f32, f32, Vec<&TextChar>)> = Vec::new();
    for c in sorted {
        let h = height(c);
        match groups.last_mut() {
            Some((y, gh, g)) if (mid(c) - *y).abs() <= 0.5 * gh.max(h) => {
                g.push(c);
                *gh = gh.max(h);
            }
            _ => groups.push((mid(c), h, vec![c])),
        }
    }
    groups
        .into_iter()
        .map(|(_, em, mut g)| {
            g.sort_by(|a, b| a.left.total_cmp(&b.left));
            let em = em.max(4.0);
            let mut segs: Vec<Segment> = Vec::new();
            let mut prev_right = f32::NEG_INFINITY;
            for c in g {
                let gap = c.left - prev_right;
                match segs.last_mut() {
                    Some(s) if gap <= 0.9 * em => {
                        if gap > 0.18 * em {
                            s.text.push(' ');
                        }
                        s.text.push(c.ch);
                        s.x1 = s.x1.max(c.right);
                    }
                    _ => segs.push(Segment { text: c.ch.to_string(), x0: c.left, x1: c.right }),
                }
                prev_right = prev_right.max(c.right);
            }
            Line { segs }
        })
        .collect()
}

fn mid(c: &TextChar) -> f32 {
    (c.top + c.bottom) * 0.5
}

/// Высота строки по символу: кегль, если PDFium его знает, иначе рамка глифа.
fn height(c: &TextChar) -> f32 {
    let box_h = c.top - c.bottom;
    if c.size > 1.0 && c.size < 200.0 {
        c.size.max(box_h)
    } else {
        box_h
    }
}

fn overlap(a: (f32, f32), b: (f32, f32)) -> f32 {
    a.1.min(b.1) - a.0.max(b.0)
}

fn table_rows(lines: &[Line]) -> Vec<Vec<Cell>> {
    // Колонки: начинаем со строки с наибольшим числом сегментов, остальные
    // сегменты присоединяем по перекрытию или заводим новые колонки.
    let seed = lines.iter().enumerate().max_by_key(|(_, l)| l.segs.len()).map(|(i, _)| i).unwrap_or(0);
    let mut cols: Vec<(f32, f32)> = lines[seed].segs.iter().map(|s| (s.x0, s.x1)).collect();
    for (li, l) in lines.iter().enumerate() {
        if li == seed {
            continue;
        }
        for s in &l.segs {
            let span = (s.x0, s.x1);
            match cols.iter_mut().filter(|c| overlap(**c, span) > 0.0).max_by(|a, b| overlap(**a, span).total_cmp(&overlap(**b, span))) {
                Some(c) => *c = (c.0.min(span.0), c.1.max(span.1)),
                None => cols.push(span),
            }
        }
    }
    cols.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f32, f32)> = Vec::new();
    for c in cols {
        match merged.last_mut() {
            Some(m) if overlap(*m, c) > 0.0 => m.1 = m.1.max(c.1),
            _ => merged.push(c),
        }
    }
    lines
        .iter()
        .map(|l| {
            let mut texts = vec![String::new(); merged.len()];
            for s in &l.segs {
                let span = (s.x0, s.x1);
                let k = (0..merged.len()).max_by(|&a, &b| overlap(merged[a], span).total_cmp(&overlap(merged[b], span))).unwrap_or(0);
                if !texts[k].is_empty() {
                    texts[k].push(' ');
                }
                texts[k].push_str(&s.text);
            }
            texts.into_iter().map(Cell::from_text).collect()
        })
        .collect()
}

/// «1 234,56», «-12.5», «1,234.50» → число; «007», «12%», даты — остаются текстом.
pub fn parse_number(s: &str) -> Option<f64> {
    let t: String = s.chars().filter(|c| !c.is_whitespace() && *c != '\u{a0}' && *c != '\u{202f}').collect();
    let t = t.replace('−', "-");
    let body = t.strip_prefix('-').unwrap_or(&t);
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ',') || !body.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    if body.len() > 1 && body.starts_with('0') && body.as_bytes()[1].is_ascii_digit() {
        return None;
    }
    let normalized = match (body.rfind(','), body.rfind('.')) {
        // Есть и запятая, и точка: десятичный — последний, другой — разряды.
        (Some(c), Some(d)) if c > d => body.replace('.', "").replace(',', "."),
        (Some(_), Some(_)) => body.replace(',', ""),
        (Some(_), None) if body.matches(',').count() == 1 => body.replace(',', "."),
        (None, Some(_)) if body.matches('.').count() == 1 => body.to_string(),
        (None, None) => body.to_string(),
        _ => return None,
    };
    let v: f64 = normalized.parse().ok()?;
    Some(if t.starts_with('-') { -v } else { v })
}

// ---------------------------------------------------------------------------
// Запись .xlsx: zip без сжатия + минимальный набор частей SpreadsheetML.

pub fn write_xlsx(path: &Path, rows: &[Vec<Cell>]) -> std::io::Result<()> {
    let mut zip = Zip::default();
    zip.add("[Content_Types].xml", CONTENT_TYPES.as_bytes());
    zip.add("_rels/.rels", ROOT_RELS.as_bytes());
    zip.add("xl/workbook.xml", WORKBOOK.as_bytes());
    zip.add("xl/_rels/workbook.xml.rels", WORKBOOK_RELS.as_bytes());
    zip.add("xl/styles.xml", STYLES.as_bytes());
    zip.add("xl/worksheets/sheet1.xml", sheet_xml(rows).as_bytes());
    let bytes = zip.finish();
    let tmp = path.with_extension("xlsx.part");
    std::fs::File::create(&tmp)?.write_all(&bytes)?;
    std::fs::rename(&tmp, path)
}

/// Длинный текст переносится по словам в ячейке шириной не больше этой.
const MAX_COL_CHARS: usize = 60;

fn sheet_xml(rows: &[Vec<Cell>]) -> String {
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
    let mut widths = vec![8usize; ncols];
    for r in rows {
        for (j, c) in r.iter().enumerate() {
            let len = match c {
                Cell::Text(t) => t.chars().count(),
                Cell::Number(n) => format_number(*n).len(),
                Cell::Empty => 0,
            };
            widths[j] = widths[j].max(len.min(MAX_COL_CHARS));
        }
    }
    let mut x = String::with_capacity(rows.len() * 64 + 512);
    x.push_str(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#);
    x.push_str(r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols>"#);
    for (j, w) in widths.iter().enumerate() {
        x.push_str(&format!(r#"<col min="{0}" max="{0}" width="{1}" customWidth="1"/>"#, j + 1, w + 2));
    }
    x.push_str("</cols><sheetData>");
    for (i, r) in rows.iter().enumerate() {
        if r.iter().all(|c| *c == Cell::Empty) {
            continue;
        }
        x.push_str(&format!(r#"<row r="{}">"#, i + 1));
        for (j, c) in r.iter().enumerate() {
            let at = format!("{}{}", col_name(j), i + 1);
            match c {
                Cell::Empty => {}
                Cell::Number(n) => x.push_str(&format!(r#"<c r="{at}"><v>{}</v></c>"#, format_number(*n))),
                Cell::Text(t) => {
                    let style = if t.chars().count() > MAX_COL_CHARS { r#" s="1""# } else { "" };
                    x.push_str(&format!(r#"<c r="{at}"{style} t="inlineStr"><is><t xml:space="preserve">{}</t></is></c>"#, xml_escape(t)));
                }
            }
        }
        x.push_str("</row>");
    }
    x.push_str("</sheetData></worksheet>");
    x
}

fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn col_name(mut j: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (j % 26) as u8);
        if j < 26 {
            break;
        }
        j = j / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

fn xml_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            // Управляющие символы в XML 1.0 запрещены.
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            '\u{fffe}' | '\u{ffff}' => {}
            c => o.push(c),
        }
    }
    o
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#;
const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
const WORKBOOK: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Лист1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
const WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="2"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0" applyAlignment="1"><alignment wrapText="1" vertical="top"/></xf></cellXfs><cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles></styleSheet>"#;

/// Zip-архив без сжатия (метод STORED) — xlsx больше ничего не требует.
#[derive(Default)]
struct Zip {
    data: Vec<u8>,
    central: Vec<u8>,
    count: u16,
}

impl Zip {
    fn add(&mut self, name: &str, body: &[u8]) {
        let crc = crc32(body);
        let offset = self.data.len() as u32;
        let (n, len) = (name.len() as u16, body.len() as u32);
        // Время 00:00, дата 1980-01-01; флаг 0x0800 — имена в UTF-8.
        let common = |v: &mut Vec<u8>| {
            v.extend_from_slice(&20u16.to_le_bytes());
            v.extend_from_slice(&0x0800u16.to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes());
            v.extend_from_slice(&0x21u16.to_le_bytes());
            v.extend_from_slice(&crc.to_le_bytes());
            v.extend_from_slice(&len.to_le_bytes());
            v.extend_from_slice(&len.to_le_bytes());
            v.extend_from_slice(&n.to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes());
        };
        self.data.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        common(&mut self.data);
        self.data.extend_from_slice(name.as_bytes());
        self.data.extend_from_slice(body);

        self.central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        self.central.extend_from_slice(&20u16.to_le_bytes());
        common(&mut self.central);
        self.central.extend_from_slice(&[0u8; 6]); // длина комментария, диск, внутр. атрибуты
        self.central.extend_from_slice(&0u32.to_le_bytes()); // внешние атрибуты
        self.central.extend_from_slice(&offset.to_le_bytes());
        self.central.extend_from_slice(name.as_bytes());
        self.count += 1;
    }

    fn finish(mut self) -> Vec<u8> {
        let cd_offset = self.data.len() as u32;
        let cd_size = self.central.len() as u32;
        self.data.extend_from_slice(&self.central);
        self.data.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        self.data.extend_from_slice(&[0u8; 4]);
        self.data.extend_from_slice(&self.count.to_le_bytes());
        self.data.extend_from_slice(&self.count.to_le_bytes());
        self.data.extend_from_slice(&cd_size.to_le_bytes());
        self.data.extend_from_slice(&cd_offset.to_le_bytes());
        self.data.extend_from_slice(&0u16.to_le_bytes());
        self.data
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Слово из символов шириной 5 pt, кегль 10, строка на высоте `y`.
    fn word(out: &mut Vec<TextChar>, text: &str, x: f32, y: f32) {
        for (i, ch) in text.chars().enumerate() {
            let left = x + i as f32 * 5.0;
            out.push(TextChar { ch, left, right: left + 4.5, bottom: y, top: y + 7.0, size: 10.0 });
        }
    }

    #[test]
    fn table_columns_and_text_lines() {
        let mut c = Vec::new();
        word(&mut c, "Отчёт", 50.0, 700.0);
        // Таблица 3×3, третья колонка — числа разной длины.
        word(&mut c, "Имя", 50.0, 680.0);
        word(&mut c, "Отдел", 150.0, 680.0);
        word(&mut c, "Сумма", 250.0, 680.0);
        word(&mut c, "Анна", 50.0, 665.0);
        word(&mut c, "Склад", 150.0, 665.0);
        word(&mut c, "1250,5", 250.0, 665.0);
        word(&mut c, "Пётр", 50.0, 650.0);
        // пустая ячейка «Отдел»
        word(&mut c, "300", 255.0, 650.0);
        word(&mut c, "Итог", 50.0, 620.0);
        let rows = page_rows(&c);
        let t = |s: &str| Cell::Text(s.into());
        assert_eq!(rows[0], vec![t("Отчёт")]);
        assert_eq!(rows[1], vec![t("Имя"), t("Отдел"), t("Сумма")]);
        assert_eq!(rows[2], vec![t("Анна"), t("Склад"), Cell::Number(1250.5)]);
        assert_eq!(rows[3], vec![t("Пётр"), Cell::Empty, Cell::Number(300.0)]);
        assert_eq!(rows[4], vec![t("Итог")]);
    }

    #[test]
    fn words_get_spaces_columns_get_cells() {
        let mut c = Vec::new();
        word(&mut c, "Итого", 50.0, 500.0);
        word(&mut c, "по", 50.0 + 5.0 * 5.0 + 2.5, 500.0); // обычный пробел между словами
        word(&mut c, "100", 300.0, 500.0);
        word(&mut c, "Всего", 50.0, 480.0);
        word(&mut c, "7", 300.0, 480.0);
        let rows = page_rows(&c);
        assert_eq!(rows[0], vec![Cell::Text("Итого по".into()), Cell::Number(100.0)]);
        assert_eq!(rows[1], vec![Cell::Text("Всего".into()), Cell::Number(7.0)]);
    }

    #[test]
    fn numbers_parse_in_russian_and_english_formats() {
        assert_eq!(parse_number("1 234,56"), Some(1234.56));
        assert_eq!(parse_number("1,234.56"), Some(1234.56));
        assert_eq!(parse_number("1.234,56"), Some(1234.56));
        assert_eq!(parse_number("−12,5"), Some(-12.5));
        assert_eq!(parse_number("42"), Some(42.0));
        assert_eq!(parse_number("0,75"), Some(0.75));
        for s in ["007", "12%", "01.02.2024", "1,2,3", "абв", "", "-", "1.2.3"] {
            assert_eq!(parse_number(s), None, "{s}");
        }
    }

    #[test]
    fn column_names() {
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(25), "Z");
        assert_eq!(col_name(26), "AA");
        assert_eq!(col_name(701), "ZZ");
        assert_eq!(col_name(702), "AAA");
    }

    #[test]
    fn crc32_reference() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn xlsx_is_a_valid_zip_with_escaped_text() {
        let dir = std::env::temp_dir().join(format!("pdfsmith-xlsx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.xlsx");
        write_xlsx(&p, &[vec![Cell::Text("a<b & \"c\"\u{1}".into()), Cell::Number(2.5)], vec![], vec![Cell::Number(3.0)]]).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        let eocd = bytes.len() - 22;
        assert_eq!(&bytes[eocd..eocd + 4], b"PK\x05\x06");
        let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]) as usize;
        let u32_at = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize;
        assert_eq!(u16_at(eocd + 10), 6, "шесть частей");
        // Обходим центральный каталог: каждая запись указывает на свой локальный
        // заголовок с тем же именем, данные сходятся с CRC.
        let mut at = u32_at(eocd + 16);
        for _ in 0..6 {
            assert_eq!(&bytes[at..at + 4], b"PK");
            let (crc, size, nlen) = (u32_at(at + 16), u32_at(at + 24), u16_at(at + 28));
            let skip = u16_at(at + 30) + u16_at(at + 32);
            let local = u32_at(at + 42);
            let name = &bytes[at + 46..at + 46 + nlen];
            assert_eq!(&bytes[local..local + 4], b"PK");
            assert_eq!(&bytes[local + 30..local + 30 + nlen], name);
            let data = &bytes[local + 30 + nlen..local + 30 + nlen + size];
            assert_eq!(crc32(data) as usize, crc);
            at += 46 + nlen + skip;
        }
        assert_eq!(at, eocd, "каталог кончается ровно на конце архива");
        let sheet = sheet_xml(&[vec![Cell::Text("a<b & \"c\"\u{1}".into()), Cell::Number(2.5)], vec![], vec![Cell::Number(3.0)]]);
        assert!(sheet.contains("a&lt;b &amp; &quot;c&quot;</t>"));
        assert!(sheet.contains(r#"<c r="B1"><v>2.5</v></c>"#));
        assert!(sheet.contains(r#"<row r="3"><c r="A3"><v>3</v></c></row>"#));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Настоящий PDF (test_pdfs/text.pdf из Word): таблица 3×3 с числами в третьей колонке.
    #[test]
    fn real_pdf_table_keeps_three_columns() {
        let _serial = crate::render_thread::pdfium_test_lock();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let pdf = root.join("test_pdfs").join("text.pdf");
        if !root.join("pdfium.dll").exists() || !pdf.exists() {
            eprintln!("ПРОПУСК: нет pdfium.dll или test_pdfs/text.pdf");
            return;
        }
        pdfsmith_pdfium::init(Some(&root)).unwrap();
        let doc = pdfsmith_pdfium::Document::open(&pdf, None).unwrap();
        let rows = page_rows(&doc.load_page(0).unwrap().chars());
        let table: Vec<&Vec<Cell>> = rows.iter().filter(|r| r.len() == 3).collect();
        assert!(table.len() >= 3, "таблица не найдена: {rows:?}");
        assert_eq!(table[0][2], Cell::Text("Ячейка 1-3".into()));
        assert_eq!(table[1][2], Cell::Number(2501.0));
        assert_eq!(table[2][2], Cell::Number(3751.5));
    }
}
