//! Подпись как инкрементальное обновление: исходные байты не трогаются
//! (прежние подписи остаются валидными), в конец дописываются словарь
//! подписи, поле-виджет со штампом, обновлённые страница и AcroForm.

use std::collections::HashSet;

use lopdf::{dictionary, Dictionary, Document, IncrementalDocument, Object, ObjectId, StringFormat};

use crate::{appearance, SignError, SignTime};

/// Сколько байт резервируется под CMS (в /Contents — вдвое больше hex-цифр).
/// ГОСТ-подпись с цепочкой из 3–4 сертификатов занимает 5–8 КБ.
pub const CONTENTS_LEN: usize = 24 * 1024;
/// Заглушка /ByteRange: ширина с запасом под файлы до 10 ГБ.
const BR_BIG: i64 = 10_000_000_000;
const BR_PLACEHOLDER: &[u8] = b"[0 10000000000 10000000000 10000000000]";

/// Где и как выглядит видимая подпись.
pub struct SignField {
    /// Номер страницы с нуля.
    pub page: usize,
    /// Прямоугольник в пространстве страницы PDF: left, bottom, right, top.
    pub rect: [f32; 4],
    /// /Rotate страницы, четверти оборота по часовой.
    pub page_rotation: u8,
    /// Строки штампа.
    pub lines: Vec<String>,
}

/// Сведения в словаре подписи.
pub struct SignInfo {
    pub name: String,
    pub reason: String,
    pub location: String,
    pub time: SignTime,
}

/// Подписывает PDF. `sign` получает подписываемые куски файла (всё, кроме
/// /Contents) и возвращает отделённую CMS (DER).
pub fn sign_pdf<F>(base: Vec<u8>, field: &SignField, info: &SignInfo, font: &[u8], sign: F) -> Result<Vec<u8>, SignError>
where
    F: FnOnce(&[&[u8]]) -> Result<Vec<u8>, String>,
{
    let prev_len = base.len();
    let mut bytes = prepare(base, field, info, font)?;

    let mut hole = vec![b'0'; CONTENTS_LEN * 2 + 2];
    hole[0] = b'<';
    *hole.last_mut().unwrap() = b'>';
    let start = find(&bytes, &hole, prev_len).ok_or_else(|| SignError::Structure("не найдено место под подпись".into()))?;
    let end = start + hole.len();
    let br_at = find(&bytes, BR_PLACEHOLDER, prev_len).ok_or_else(|| SignError::Structure("не найден /ByteRange".into()))?;

    let mut br = format!("[0 {start} {end} {}", bytes.len() - end).into_bytes();
    br.resize(BR_PLACEHOLDER.len() - 1, b' ');
    br.push(b']');
    bytes[br_at..br_at + br.len()].copy_from_slice(&br);

    let cms = sign(&[&bytes[..start], &bytes[end..]]).map_err(SignError::Signer)?;
    if cms.len() > CONTENTS_LEN {
        return Err(SignError::TooLarge(cms.len()));
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for (i, b) in cms.iter().enumerate() {
        bytes[start + 1 + 2 * i] = HEX[(b >> 4) as usize];
        bytes[start + 2 + 2 * i] = HEX[(b & 15) as usize];
    }
    Ok(bytes)
}

/// Дописывает обновление с заглушками /ByteRange и /Contents.
fn prepare(base: Vec<u8>, field: &SignField, info: &SignInfo, font: &[u8]) -> Result<Vec<u8>, SignError> {
    let doc = Document::load_mem(&base).map_err(|e| SignError::Parse(e.to_string()))?;
    if doc.is_encrypted() || doc.encryption_state.is_some() || doc.trailer.get(b"Encrypt").is_ok() {
        return Err(SignError::Encrypted);
    }
    let page_id = *doc.get_pages().get(&(field.page as u32 + 1)).ok_or(SignError::NoPage(field.page))?;
    let root_id = doc.trailer.get(b"Root").and_then(Object::as_reference)?;
    let field_name = unique_field_name(&doc, root_id);

    let mut inc = IncrementalDocument::create_from(base, doc);
    // Трейлер скопирован из прежней ревизии: гибридный /XRefStm указывал бы
    // мимо, а /DecodeParms старого потока xref испортил бы чтение нового.
    inc.new_document.trailer.remove(b"XRefStm");
    inc.new_document.trailer.remove(b"DecodeParms");
    let nd = &mut inc.new_document;

    let ap_id = appearance::build(nd, field, font)?;
    let mut sig = dictionary! {
        "Type" => "Sig",
        "Filter" => "Adobe.PPKLite",
        "SubFilter" => "adbe.pkcs7.detached",
        "ByteRange" => vec![0.into(), BR_BIG.into(), BR_BIG.into(), BR_BIG.into()],
        "Contents" => Object::String(vec![0; CONTENTS_LEN], StringFormat::Hexadecimal),
        "M" => Object::string_literal(info.time.pdf_date()),
    };
    for (key, val) in [("Name", &info.name), ("Reason", &info.reason), ("Location", &info.location)] {
        if !val.trim().is_empty() {
            sig.set(key, text_string(val.trim()));
        }
    }
    let sig_id = nd.add_object(sig);
    let [l, b, r, t] = field.rect;
    let widget = dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Sig",
        "T" => Object::string_literal(field_name),
        "V" => sig_id,
        // Print | Locked
        "F" => 132,
        "Rect" => vec![Object::Real(l), Object::Real(b), Object::Real(r), Object::Real(t)],
        "P" => page_id,
        "AP" => dictionary! { "N" => ap_id },
    };
    let widget_id = nd.add_object(widget);

    add_annot(&mut inc, page_id, widget_id)?;
    add_field(&mut inc, root_id, widget_id)?;

    let mut out = Vec::new();
    inc.save_to(&mut out).map_err(|e| SignError::Structure(e.to_string()))?;
    Ok(out)
}

/// Виджет — в /Annots страницы (массив бывает прямым и косвенным).
fn add_annot(inc: &mut IncrementalDocument, page_id: ObjectId, widget_id: ObjectId) -> Result<(), SignError> {
    inc.opt_clone_object_to_new_document(page_id)?;
    let annots_ref = match inc.new_document.get_dictionary(page_id)?.get(b"Annots") {
        Ok(Object::Reference(id)) => Some(*id),
        _ => None,
    };
    if let Some(id) = annots_ref {
        inc.opt_clone_object_to_new_document(id)?;
        inc.new_document.get_object_mut(id)?.as_array_mut()?.push(widget_id.into());
    } else {
        let page = inc.new_document.get_dictionary_mut(page_id)?;
        match page.get_mut(b"Annots") {
            Ok(Object::Array(a)) => a.push(widget_id.into()),
            _ => page.set("Annots", vec![widget_id.into()]),
        }
    }
    Ok(())
}

/// Поле — в /AcroForm каталога, /SigFlags 3 (есть подписи, только дописывать).
fn add_field(inc: &mut IncrementalDocument, root_id: ObjectId, widget_id: ObjectId) -> Result<(), SignError> {
    inc.opt_clone_object_to_new_document(root_id)?;
    let form_ref = match inc.new_document.get_dictionary(root_id)?.get(b"AcroForm") {
        Ok(Object::Reference(id)) => Some(*id),
        _ => None,
    };
    if let Some(id) = form_ref {
        inc.opt_clone_object_to_new_document(id)?;
    }
    let fields_ref = match form_dict(&mut inc.new_document, root_id, form_ref)?.get(b"Fields") {
        Ok(Object::Reference(id)) => Some(*id),
        _ => None,
    };
    if let Some(id) = fields_ref {
        inc.opt_clone_object_to_new_document(id)?;
        inc.new_document.get_object_mut(id)?.as_array_mut()?.push(widget_id.into());
    }
    let form = form_dict(&mut inc.new_document, root_id, form_ref)?;
    if fields_ref.is_none() {
        match form.get_mut(b"Fields") {
            Ok(Object::Array(a)) => a.push(widget_id.into()),
            _ => form.set("Fields", vec![widget_id.into()]),
        }
    }
    form.set("SigFlags", 3);
    Ok(())
}

fn form_dict(doc: &mut Document, root_id: ObjectId, form_ref: Option<ObjectId>) -> Result<&mut Dictionary, SignError> {
    if let Some(id) = form_ref {
        return Ok(doc.get_dictionary_mut(id)?);
    }
    let catalog = doc.get_dictionary_mut(root_id)?;
    if !matches!(catalog.get(b"AcroForm"), Ok(Object::Dictionary(_))) {
        catalog.set("AcroForm", Dictionary::new());
    }
    Ok(catalog.get_mut(b"AcroForm")?.as_dict_mut()?)
}

/// `Signature1`, `Signature2`… — первое имя, которого ещё нет среди полей.
fn unique_field_name(doc: &Document, root_id: ObjectId) -> String {
    let resolve = |o: &Object| -> Option<Object> {
        match o {
            Object::Reference(id) => doc.get_object(*id).ok().cloned(),
            other => Some(other.clone()),
        }
    };
    let mut taken = HashSet::new();
    let fields = doc
        .get_dictionary(root_id)
        .ok()
        .and_then(|c| c.get(b"AcroForm").ok().and_then(resolve))
        .and_then(|f| f.as_dict().ok().and_then(|d| d.get(b"Fields").ok().and_then(resolve)));
    if let Some(Object::Array(items)) = fields {
        for item in &items {
            if let Some(Object::Dictionary(d)) = resolve(item) {
                if let Ok(Object::String(s, _)) = d.get(b"T") {
                    taken.insert(String::from_utf8_lossy(s).to_string());
                }
            }
        }
    }
    (1..).map(|i| format!("Signature{i}")).find(|n| !taken.contains(n)).unwrap()
}

/// Текстовая строка PDF: ASCII как есть, иначе UTF-16BE с BOM.
fn text_string(s: &str) -> Object {
    if s.is_ascii() {
        return Object::string_literal(s);
    }
    let mut v = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        v.extend(u.to_be_bytes());
    }
    Object::String(v, StringFormat::Hexadecimal)
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let first = needle[0];
    let mut i = from;
    while i + needle.len() <= hay.len() {
        let off = hay[i..hay.len() - needle.len() + 1].iter().position(|&b| b == first)?;
        i += off;
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}
