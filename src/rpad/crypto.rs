//! S3 客户端加密用到的两种解密算法。
//!
//! * `aes_gcm_decrypt`：对应 Python 的 `AES.new(key, AES.MODE_GCM, iv).decrypt_and_verify(...)`
//!   —— `mfront` / `crm` / `crm_v2` 的对象都是「AES-GCM 密文 + 末尾 16 字节 tag」。
//! * `fernet_decrypt`：对应 Python 的 `cryptography.fernet.Fernet`——`hhcrm` 用。

use aes::Aes128;
use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce, Tag};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE as B64_URL_SAFE;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// AES-256-GCM 解密（Python 的 `AES.MODE_GCM` 默认就是 128 bit tag）。
///
/// `ct` 是密文（不含 tag），`tag` 是末尾那 16 字节。
pub fn aes_gcm_decrypt(key: &[u8], iv: &[u8], ct: &[u8], tag: &[u8]) -> Result<Vec<u8>, String> {
    if key.len() != 32 && key.len() != 16 {
        return Err(format!(
            "AES-GCM 数据密钥长度异常：{} 字节（期望 16 或 32）",
            key.len()
        ));
    }
    if iv.len() != 12 {
        // s3crypto 固定用 12 字节 IV；长度不对时 Nonce::from_slice 会 panic
        return Err(format!("AES-GCM IV 长度异常：{} 字节（期望 12）", iv.len()));
    }
    if tag.len() != 16 {
        return Err(format!("AES-GCM tag 长度异常：{} 字节（期望 16）", tag.len()));
    }

    let nonce = Nonce::from_slice(iv);
    let aad: &[u8] = &[];
    match key.len() {
        32 => {
            let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| format!("AES-GCM 密钥无效: {e}"))?;
            let mut buf = ct.to_vec();
            cipher
                .decrypt_in_place_detached(nonce, aad, &mut buf, Tag::from_slice(tag))
                .map_err(|_| "AES-GCM 校验失败（tag 不匹配，密钥或数据不对）".to_string())?;
            Ok(buf)
        }
        // AES-128-GCM：AWS 的 KMS 数据密钥永远是 256 bit，这里是兜底。
        _ => {
            use aes_gcm::Aes128Gcm;
            let cipher = Aes128Gcm::new_from_slice(key).map_err(|e| format!("AES-GCM 密钥无效: {e}"))?;
            let mut buf = ct.to_vec();
            cipher
                .decrypt_in_place_detached(nonce, aad, &mut buf, Tag::from_slice(tag))
                .map_err(|_| "AES-GCM 校验失败（tag 不匹配，密钥或数据不对）".to_string())?;
            Ok(buf)
        }
    }
}

/// Fernet 解密（AES-128-CBC + HMAC-SHA256），token 是 `base64url(...)` 的 ASCII 文本。
///
/// 结构：`version(1) || timestamp(8) || iv(16) || ciphertext || hmac(32)`，
/// 签名密钥是 32 字节密钥的前 16 字节，加密密钥是后 16 字节。
pub fn fernet_decrypt(raw_key: &[u8], token: &[u8]) -> Result<Vec<u8>, String> {
    if raw_key.len() != 32 {
        return Err(format!(
            "Fernet 密钥长度异常：{} 字节（期望 32）",
            raw_key.len()
        ));
    }
    let text = std::str::from_utf8(token)
        .map_err(|e| format!("Fernet token 不是 UTF-8 文本: {e}"))?
        .trim();
    let data = B64_URL_SAFE
        .decode(text.as_bytes())
        .map_err(|e| format!("Fernet token base64 解码失败: {e}"))?;

    if data.len() < 1 + 8 + 16 + 32 {
        return Err(format!("Fernet token 太短：{} 字节", data.len()));
    }
    if data[0] != 0x80 {
        return Err(format!("Fernet version 不是 0x80（实际 0x{:02x}）", data[0]));
    }

    let (payload, sig) = data.split_at(data.len() - 32);
    let signing_key = &raw_key[..16];
    let enc_key = &raw_key[16..];

    let mut mac = <HmacSha256 as Mac>::new_from_slice(signing_key).expect("HMAC 接受任意长度密钥");
    mac.update(payload);
    let expect = mac.finalize().into_bytes();
    if expect.as_slice() != sig {
        return Err("Fernet HMAC 校验失败（数据被篡改或密钥不对）".to_string());
    }

    let iv = &payload[9..25];
    let ct = &payload[25..];
    let mut buf = ct.to_vec();
    let dec = cbc::Decryptor::<Aes128>::new_from_slices(enc_key, iv)
        .map_err(|e| format!("Fernet AES-128-CBC 初始化失败: {e}"))?;
    let plain = dec
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| format!("Fernet PKCS7 去填充失败: {e}"))?;
    Ok(plain.to_vec())
}
