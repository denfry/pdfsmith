//! Подпись без токена: подписант-заглушка возвращает фиксированные байты, а
//! тест проверяет структуру обновления и /ByteRange.

use lopdf::{dictionary, Document, Object, SaveOptions, Stream};
use pdfsmith_sign::{sign_pdf, SignField, SignInfo, SignTime};

fn font() -> Option<Vec<u8>> {
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    ["arial.ttf", "segoeui.ttf", "tahoma.ttf"]
        .iter()
        .find_map(|n| std::fs::read(std::path::Path::new(&dir).join("Fonts").join(n)).ok())
}

/// Двухстраничный PDF; вторая страница с /Rotate 90 и косвенным /Annots.
fn sample(modern: bool) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let content = doc.add_object(Stream::new(dictionary! {}, b"BT ET".to_vec()));
    let annots = doc.add_object(Vec::<Object>::new());
    let p1 = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    });
    let p2 = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content, "Rotate" => 90, "Annots" => annots,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    });
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![p1.into(), p2.into()], "Count" => 2 }));
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut out = Vec::new();
    if modern {
        let opts = SaveOptions::builder().use_object_streams(true).use_xref_streams(true).build();
        doc.save_with_options(&mut out, opts).unwrap();
    } else {
        doc.save_to(&mut out).unwrap();
    }
    out
}

fn info() -> SignInfo {
    SignInfo {
        name: "Иванов Иван".into(),
        reason: "Согласовано".into(),
        location: String::new(),
        time: SignTime { year: 2026, month: 10, day: 20, hour: 15, minute: 30, second: 0, offset_min: 180 },
    }
}

fn field(page: usize, rotation: u8) -> SignField {
    SignField {
        page,
        rect: [50.0, 50.0, 250.0, 120.0],
        page_rotation: rotation,
        lines: vec!["Подписано электронной подписью".into(), "Иванов Иван".into(), "Дата: 20.10.2026 15:30:00 +03:00".into()],
    }
}

/// Подписывает, проверяет ByteRange/Contents и возвращает перечитанный документ.
fn sign_and_check(base: Vec<u8>, f: &SignField) -> (Vec<u8>, Document) {
    let font = font().expect("системный шрифт");
    let cms = vec![0x30, 0x82, 0xAB, 0xCD];
    let mut seen = Vec::new();
    let out = sign_pdf(base.clone(), f, &info(), &font, |chunks| {
        seen = chunks.concat();
        Ok(cms.clone())
    })
    .expect("подпись");
    assert!(out.starts_with(&base), "исходные байты должны остаться префиксом");

    let doc = Document::load_mem(&out).expect("подписанный PDF читается");
    let catalog = doc.catalog().unwrap();
    let form = match catalog.get(b"AcroForm").unwrap() {
        Object::Reference(id) => doc.get_dictionary(*id).unwrap(),
        Object::Dictionary(d) => d,
        _ => panic!("AcroForm"),
    };
    assert_eq!(form.get(b"SigFlags").unwrap().as_i64().unwrap(), 3);
    let fields = form.get(b"Fields").unwrap().as_array().unwrap();
    let widget_id = fields.last().unwrap().as_reference().unwrap();
    let widget = doc.get_dictionary(widget_id).unwrap();
    assert_eq!(widget.get(b"FT").unwrap().as_name().unwrap(), b"Sig");
    let sig = doc.get_dictionary(widget.get(b"V").unwrap().as_reference().unwrap()).unwrap();
    assert_eq!(sig.get(b"SubFilter").unwrap().as_name().unwrap(), b"adbe.pkcs7.detached");

    let br: Vec<usize> = sig.get(b"ByteRange").unwrap().as_array().unwrap().iter().map(|o| o.as_i64().unwrap() as usize).collect();
    assert_eq!(br[0], 0);
    assert_eq!(br[2] + br[3], out.len(), "ByteRange покрывает файл до конца");
    assert_eq!(out[br[1]], b'<');
    assert_eq!(out[br[2] - 1], b'>');
    let mut signed = out[..br[1]].to_vec();
    signed.extend_from_slice(&out[br[2]..]);
    assert_eq!(seen, signed, "подписаны ровно байты вне /Contents");

    let contents = sig.get(b"Contents").unwrap().as_str().unwrap();
    assert_eq!(&contents[..4], &cms[..]);
    assert!(contents[4..].iter().all(|&b| b == 0));

    // Виджет попал в /Annots своей страницы.
    let page_id = doc.get_pages()[&(f.page as u32 + 1)];
    let annots = match doc.get_dictionary(page_id).unwrap().get(b"Annots").unwrap() {
        Object::Reference(id) => doc.get_object(*id).unwrap().as_array().unwrap().clone(),
        Object::Array(a) => a.clone(),
        _ => panic!("Annots"),
    };
    assert!(annots.contains(&Object::Reference(widget_id)));
    (out, doc)
}

#[test]
fn signs_classic_xref() {
    if font().is_none() {
        return;
    }
    sign_and_check(sample(false), &field(0, 0));
}

#[test]
fn signs_xref_stream_and_rotated_page() {
    if font().is_none() {
        return;
    }
    let (_, doc) = sign_and_check(sample(true), &field(1, 1));
    let page_id = doc.get_pages()[&2];
    assert!(matches!(doc.get_dictionary(page_id).unwrap().get(b"Annots").unwrap(), Object::Reference(_)));
}

#[test]
fn second_signature_keeps_first() {
    if font().is_none() {
        return;
    }
    let (first, _) = sign_and_check(sample(false), &field(0, 0));
    let (second, doc) = sign_and_check(first.clone(), &field(0, 0));
    assert!(second.starts_with(&first));
    let catalog = doc.catalog().unwrap();
    let form = catalog.get(b"AcroForm").unwrap().as_dict().unwrap();
    let names: Vec<Vec<u8>> = form
        .get(b"Fields")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|r| doc.get_dictionary(r.as_reference().unwrap()).unwrap().get(b"T").unwrap().as_str().unwrap().to_vec())
        .collect();
    assert_eq!(names, vec![b"Signature1".to_vec(), b"Signature2".to_vec()]);
}

#[test]
fn oversized_signature_is_rejected() {
    let Some(font) = font() else { return };
    let r = sign_pdf(sample(false), &field(0, 0), &info(), &font, |_| Ok(vec![1; pdfsmith_sign::CONTENTS_LEN + 1]));
    assert!(matches!(r, Err(pdfsmith_sign::SignError::TooLarge(_))));
}

/// Подписанный файл открывает PDFium, штамп виден как аннотация и рисуется.
#[test]
fn signed_pdf_opens_in_pdfium() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    if !root.join("pdfium.dll").exists() || font().is_none() {
        eprintln!("ПРОПУСК: нет pdfium.dll или шрифта");
        return;
    }
    pdfsmith_pdfium::init(Some(&root)).expect("pdfium.dll");
    let (out, _) = sign_and_check(sample(false), &field(0, 0));
    let doc = pdfsmith_pdfium::Document::from_bytes(out, None).expect("PDFium открывает подписанный файл");
    assert_eq!(doc.page_count(), 2);
    assert_eq!(doc.load_page(0).unwrap().annotation_count(), 1);
    // В области штампа есть тёмные пиксели (текст и рамка), вне — белое.
    let img = doc.render_page_scaled(0, 1.0).unwrap();
    let px = |x: u32, y: u32| {
        let i = ((y * img.width + x) * 4) as usize;
        img.rgba[i] as u32 + img.rgba[i + 1] as u32 + img.rgba[i + 2] as u32
    };
    let h = img.height;
    let dark = (55..245).flat_map(|x| ((h - 118)..(h - 52)).map(move |y| (x, y))).filter(|&(x, y)| px(x, y) < 450).count();
    assert!(dark > 200, "штамп не нарисовался: тёмных пикселей {dark}");
    assert_eq!(px(400, 100), 765);
}

/// Чтение хранилища не требует токена и не спрашивает PIN.
#[cfg(windows)]
#[test]
fn lists_certificates() {
    let certs = pdfsmith_sign::store::list_certificates().expect("хранилище «Личное»");
    for c in &certs {
        eprintln!("{} | {} | до {} | истёк: {}", c.owner, c.issuer, c.valid_to, c.expired);
        assert_eq!(c.thumbprint.len(), 20);
    }
    let now = pdfsmith_sign::store::now();
    eprintln!("сейчас: {}", now.local.display());
}

/// Настоящая подпись сертификатом из «Личного» (отпечаток hex в
/// `PDFSMITH_TEST_CERT`) и проверка CMS средствами Windows. Ключ на токене
/// спросит PIN, поэтому по умолчанию тест пропускается.
#[cfg(windows)]
#[test]
fn signs_with_store_certificate() {
    use windows_sys::Win32::Security::Cryptography::*;

    let Ok(thumb_hex) = std::env::var("PDFSMITH_TEST_CERT") else {
        eprintln!("ПРОПУСК: не задан PDFSMITH_TEST_CERT");
        return;
    };
    let Some(font) = font() else { return };
    let thumb: Vec<u8> = (0..thumb_hex.len()).step_by(2).map(|i| u8::from_str_radix(&thumb_hex[i..i + 2], 16).unwrap()).collect();
    let now = pdfsmith_sign::store::now();
    let mut cms = Vec::new();
    let out = sign_pdf(sample(false), &field(0, 0), &SignInfo { time: now.local, ..info() }, &font, |chunks| {
        let r = pdfsmith_sign::store::sign_detached(&thumb, chunks, now.utc_filetime);
        if let Ok(c) = &r {
            cms = c.clone();
        }
        r
    })
    .expect("подпись");
    let doc = Document::load_mem(&out).unwrap();
    let sig = doc
        .objects
        .values()
        .find_map(|o| o.as_dict().ok().filter(|d| d.get(b"ByteRange").is_ok()))
        .expect("словарь подписи");
    let br: Vec<usize> = sig.get(b"ByteRange").unwrap().as_array().unwrap().iter().map(|o| o.as_i64().unwrap() as usize).collect();
    let chunks = [&out[br[0]..br[0] + br[1]], &out[br[2]..br[2] + br[3]]];
    let ptrs: Vec<*const u8> = chunks.iter().map(|c| c.as_ptr()).collect();
    let lens: Vec<u32> = chunks.iter().map(|c| c.len() as u32).collect();
    let mut para: CRYPT_VERIFY_MESSAGE_PARA = unsafe { std::mem::zeroed() };
    para.cbSize = std::mem::size_of::<CRYPT_VERIFY_MESSAGE_PARA>() as u32;
    para.dwMsgAndCertEncodingType = X509_ASN_ENCODING | PKCS_7_ASN_ENCODING;
    let ok = unsafe {
        CryptVerifyDetachedMessageSignature(&para, 0, cms.as_ptr(), cms.len() as u32, 2, ptrs.as_ptr(), lens.as_ptr(), std::ptr::null_mut())
    };
    assert_ne!(ok, 0, "CMS не прошла проверку: {}", std::io::Error::last_os_error());
    std::fs::write(std::env::temp_dir().join("pdfsmith-signed-test.pdf"), &out).unwrap();
    eprintln!("CMS {} байт проверена, файл: {}", cms.len(), std::env::temp_dir().join("pdfsmith-signed-test.pdf").display());
}
