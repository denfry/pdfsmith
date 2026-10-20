//! Сертификаты из хранилища Windows «Личное» и отделённая CMS-подпись через
//! CryptoAPI. Ключи на токенах КриптоПро CSP доступны так же, как любые
//! другие: PIN-код спрашивает сам CSP своим окном.

use std::ffi::CStr;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{GetLastError, FILETIME, SYSTEMTIME};
use windows_sys::Win32::Security::Cryptography::*;
use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime};

use crate::{CertInfo, Now, SignTime};

const ENCODING: u32 = X509_ASN_ENCODING | PKCS_7_ASN_ENCODING;
const OID_SIGNING_TIME: &CStr = c"1.2.840.113549.1.9.5";

pub fn now() -> Now {
    unsafe {
        let mut ft: FILETIME = zeroed();
        GetSystemTimeAsFileTime(&mut ft);
        // Отбрасываем доли секунды, чтобы штамп и CMS совпадали.
        let utc_filetime = ft_u64(ft) / 10_000_000 * 10_000_000;
        let utc = to_systemtime(utc_filetime);
        let mut local: SYSTEMTIME = zeroed();
        if SystemTimeToTzSpecificLocalTime(null(), &utc, &mut local) == 0 {
            local = utc;
        }
        let mut lft: FILETIME = zeroed();
        SystemTimeToFileTime(&local, &mut lft);
        let offset_min = ((ft_u64(lft) as i64 - utc_filetime as i64) as f64 / 600_000_000.0).round() as i32;
        Now {
            local: SignTime {
                year: local.wYear,
                month: local.wMonth as u8,
                day: local.wDay as u8,
                hour: local.wHour as u8,
                minute: local.wMinute as u8,
                second: local.wSecond as u8,
                offset_min,
            },
            utc_filetime,
        }
    }
}

/// Сертификаты из «Личного» хранилища текущего пользователя, к которым
/// привязан закрытый ключ.
pub fn list_certificates() -> Result<Vec<CertInfo>, String> {
    let store = Store::open_my()?;
    let now = now().utc_filetime;
    let mut out = Vec::new();
    unsafe {
        let mut ctx: *const CERT_CONTEXT = null();
        loop {
            // Предыдущий контекст функция освобождает сама.
            ctx = CertEnumCertificatesInStore(store.0, ctx);
            if ctx.is_null() {
                break;
            }
            let mut len = 0u32;
            if CertGetCertificateContextProperty(ctx, CERT_KEY_PROV_INFO_PROP_ID, null_mut(), &mut len) == 0 {
                continue;
            }
            out.push(cert_info(ctx, now));
        }
    }
    out.sort_by(|a, b| a.expired.cmp(&b.expired).then_with(|| a.owner.to_lowercase().cmp(&b.owner.to_lowercase())));
    Ok(out)
}

/// Отделённая CMS (PKCS#7 SignedData без содержимого) над `chunks` ключом
/// сертификата `thumbprint`. Хэш — по алгоритму ключа (ГОСТ Р 34.11-2012 для
/// ГОСТ-ключей), в подписанных атрибутах — время подписи.
pub fn sign_detached(thumbprint: &[u8], chunks: &[&[u8]], utc_filetime: u64) -> Result<Vec<u8>, String> {
    let store = Store::open_my()?;
    let cert = store.find(thumbprint).ok_or("сертификат не найден в хранилище «Личное»")?;
    unsafe {
        let info = &*(*cert.0).pCertInfo;
        let key_oid = CStr::from_ptr(info.SubjectPublicKeyInfo.Algorithm.pszObjId as *const _).to_string_lossy();
        let hash_oid = hash_for_key(&key_oid);

        // Цепочка до корня — чтобы проверяющему не пришлось её искать.
        let mut certs: Vec<*mut CERT_CONTEXT> = vec![cert.0];
        let mut chain: *mut CERT_CHAIN_CONTEXT = null_mut();
        let mut chain_para: CERT_CHAIN_PARA = zeroed();
        chain_para.cbSize = size_of::<CERT_CHAIN_PARA>() as u32;
        if CertGetCertificateChain(0, cert.0, null(), null_mut(), &chain_para, 0, null(), &mut chain) != 0 && !chain.is_null() {
            let c = &*chain;
            if c.cChain > 0 {
                let simple = &**c.rgpChain;
                for i in 1..simple.cElement as usize {
                    let el = &**simple.rgpElement.add(i);
                    certs.push(el.pCertContext as *mut _);
                }
            }
        }

        let ft = FILETIME { dwLowDateTime: utc_filetime as u32, dwHighDateTime: (utc_filetime >> 32) as u32 };
        let result = (|| {
            let time = encode(PKCS_UTC_TIME, &ft as *const _ as *const _)?;
            let mut time_blob = CRYPT_INTEGER_BLOB { cbData: time.len() as u32, pbData: time.as_ptr() as *mut u8 };
            let mut attr = CRYPT_ATTRIBUTE { pszObjId: OID_SIGNING_TIME.as_ptr() as *mut u8, cValue: 1, rgValue: &mut time_blob };
            let mut para: CRYPT_SIGN_MESSAGE_PARA = zeroed();
            para.cbSize = size_of::<CRYPT_SIGN_MESSAGE_PARA>() as u32;
            para.dwMsgEncodingType = ENCODING;
            para.pSigningCert = cert.0;
            para.HashAlgorithm.pszObjId = hash_oid.as_ptr() as *mut u8;
            para.cMsgCert = certs.len() as u32;
            para.rgpMsgCert = certs.as_mut_ptr();
            para.cAuthAttr = 1;
            para.rgAuthAttr = &mut attr;

            let ptrs: Vec<*const u8> = chunks.iter().map(|c| c.as_ptr()).collect();
            let lens: Vec<u32> = chunks.iter().map(|c| c.len() as u32).collect();
            // Сразу с запасом: запрос размера у некоторых CSP подписывает
            // по-настоящему и спросил бы PIN дважды.
            let mut out = vec![0u8; 64 * 1024];
            let mut size = out.len() as u32;
            let mut ok = CryptSignMessage(&para, 1, chunks.len() as u32, ptrs.as_ptr(), lens.as_ptr(), out.as_mut_ptr(), &mut size);
            if ok == 0 && GetLastError() == 234 {
                // ERROR_MORE_DATA: size уже содержит нужный размер.
                out.resize(size as usize, 0);
                ok = CryptSignMessage(&para, 1, chunks.len() as u32, ptrs.as_ptr(), lens.as_ptr(), out.as_mut_ptr(), &mut size);
            }
            if ok == 0 {
                return Err(sign_error(GetLastError()));
            }
            out.truncate(size as usize);
            Ok(out)
        })();
        if !chain.is_null() {
            CertFreeCertificateChain(chain);
        }
        result
    }
}

/// OID хэша под алгоритм открытого ключа.
fn hash_for_key(key_oid: &str) -> &'static CStr {
    match key_oid {
        // ГОСТ Р 34.10-2012 256 → ГОСТ Р 34.11-2012 256
        "1.2.643.7.1.1.1.1" => c"1.2.643.7.1.1.2.2",
        // ГОСТ Р 34.10-2012 512 → ГОСТ Р 34.11-2012 512
        "1.2.643.7.1.1.1.2" => c"1.2.643.7.1.1.2.3",
        // ГОСТ Р 34.10-2001 → ГОСТ Р 34.11-94
        "1.2.643.2.2.19" => c"1.2.643.2.2.9",
        // RSA/ECDSA → SHA-256
        _ => c"2.16.840.1.101.3.4.2.1",
    }
}

fn sign_error(code: u32) -> String {
    match code {
        // ERROR_CANCELLED, SCARD_W_CANCELLED_BY_USER
        1223 | 0x8010_006E => "Подпись отменена".into(),
        // SCARD_W_WRONG_CHV
        0x8010_006B => "Неверный PIN-код".into(),
        // SCARD_W_CHV_BLOCKED
        0x8010_006C => "PIN-код заблокирован".into(),
        // NTE_BAD_KEYSET, NTE_KEYSET_NOT_DEF, SCARD_E_NO_SMARTCARD
        0x8009_0016 | 0x8009_0019 | 0x8010_000C => "Ключ не найден: вставьте токен с этим сертификатом".into(),
        _ => format!("Не удалось подписать: {} (0x{code:08X})", std::io::Error::from_raw_os_error(code as i32)),
    }
}

unsafe fn encode(struct_type: windows_sys::core::PCSTR, value: *const core::ffi::c_void) -> Result<Vec<u8>, String> {
    let mut len = 0u32;
    if CryptEncodeObjectEx(X509_ASN_ENCODING, struct_type, value, 0, null(), null_mut(), &mut len) == 0 {
        return Err(sign_error(GetLastError()));
    }
    let mut buf = vec![0u8; len as usize];
    if CryptEncodeObjectEx(X509_ASN_ENCODING, struct_type, value, 0, null(), buf.as_mut_ptr() as *mut _, &mut len) == 0 {
        return Err(sign_error(GetLastError()));
    }
    buf.truncate(len as usize);
    Ok(buf)
}

unsafe fn cert_info(ctx: *const CERT_CONTEXT, now: u64) -> CertInfo {
    let info = &*(*ctx).pCertInfo;
    let mut thumbprint = vec![0u8; 20];
    let mut len = thumbprint.len() as u32;
    if CertGetCertificateContextProperty(ctx, CERT_HASH_PROP_ID, thumbprint.as_mut_ptr() as *mut _, &mut len) != 0 {
        thumbprint.truncate(len as usize);
    } else {
        thumbprint.clear();
    }
    let surname = name(ctx, CERT_NAME_ATTR_TYPE, 0, c"2.5.4.4");
    let given = name(ctx, CERT_NAME_ATTR_TYPE, 0, c"2.5.4.42");
    let owner = if !surname.is_empty() && !given.is_empty() {
        format!("{surname} {given}")
    } else {
        name(ctx, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, c"")
    };
    let serial = std::slice::from_raw_parts(info.SerialNumber.pbData, info.SerialNumber.cbData as usize)
        .iter()
        .rev()
        .map(|b| format!("{b:02X}"))
        .collect();
    let (from, to) = (ft_u64(info.NotBefore), ft_u64(info.NotAfter));
    CertInfo {
        thumbprint,
        owner,
        organization: name(ctx, CERT_NAME_ATTR_TYPE, 0, c"2.5.4.10"),
        issuer: name(ctx, CERT_NAME_SIMPLE_DISPLAY_TYPE, CERT_NAME_ISSUER_FLAG, c""),
        serial,
        valid_from: local_date(from),
        valid_to: local_date(to),
        expired: now < from || now > to,
    }
}

/// `CertGetNameStringW`; для `CERT_NAME_ATTR_TYPE` в `oid` — OID атрибута.
unsafe fn name(ctx: *const CERT_CONTEXT, kind: u32, flags: u32, oid: &CStr) -> String {
    let para = if kind == CERT_NAME_ATTR_TYPE { oid.as_ptr() as *const core::ffi::c_void } else { null() };
    let n = CertGetNameStringW(ctx, kind, flags, para, null_mut(), 0);
    if n <= 1 {
        return String::new();
    }
    let mut buf = vec![0u16; n as usize];
    let n = CertGetNameStringW(ctx, kind, flags, para, buf.as_mut_ptr(), n);
    String::from_utf16_lossy(&buf[..(n as usize).saturating_sub(1)])
}

fn ft_u64(ft: FILETIME) -> u64 {
    (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64
}

fn to_systemtime(t: u64) -> SYSTEMTIME {
    let ft = FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 };
    unsafe {
        let mut st: SYSTEMTIME = zeroed();
        FileTimeToSystemTime(&ft, &mut st);
        st
    }
}

fn local_date(t: u64) -> String {
    let utc = to_systemtime(t);
    let mut st: SYSTEMTIME = unsafe { zeroed() };
    if unsafe { SystemTimeToTzSpecificLocalTime(null(), &utc, &mut st) } == 0 {
        st = utc;
    }
    format!("{:02}.{:02}.{:04}", st.wDay, st.wMonth, st.wYear)
}

struct Store(HCERTSTORE);

impl Store {
    fn open_my() -> Result<Store, String> {
        let name: Vec<u16> = "MY\0".encode_utf16().collect();
        let h = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                0,
                0,
                CERT_SYSTEM_STORE_CURRENT_USER | CERT_STORE_READONLY_FLAG | CERT_STORE_OPEN_EXISTING_FLAG,
                name.as_ptr() as *const _,
            )
        };
        if h.is_null() {
            return Err(format!("не открылось хранилище сертификатов: {}", std::io::Error::last_os_error()));
        }
        Ok(Store(h))
    }

    fn find(&self, thumbprint: &[u8]) -> Option<Cert> {
        let blob = CRYPT_INTEGER_BLOB { cbData: thumbprint.len() as u32, pbData: thumbprint.as_ptr() as *mut u8 };
        let ctx = unsafe { CertFindCertificateInStore(self.0, ENCODING, 0, CERT_FIND_HASH, &blob as *const _ as *const _, null()) };
        (!ctx.is_null()).then_some(Cert(ctx))
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        unsafe { CertCloseStore(self.0, 0) };
    }
}

struct Cert(*mut CERT_CONTEXT);

impl Drop for Cert {
    fn drop(&mut self) {
        unsafe { CertFreeCertificateContext(self.0) };
    }
}
