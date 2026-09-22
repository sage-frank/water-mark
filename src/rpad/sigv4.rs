//! 最小实现的 AWS Signature Version 4 签名。
//!
//! 为什么自己写：AWS SDK for Rust 的 HTTPS 客户端只有 rustls（aws-lc-rs，构建需要
//! cmake）和 ring（Windows 构建需要 nasm）两种，本机的构建环境下都编不出来。
//! 而这里只需要「给 S3 GET / KMS POST 各签一次名」，签名算法本身不到 200 行，
//! 比拖一整套 SDK 依赖更可控。
//!
//! 参考：<https://docs.aws.amazon.com/IAM/latest/UserGuide/create-signed-request.html>
//!
//! 使用方式见 [`sign_request`]：把待签名的头交给它，拿到「还要补到请求上的头」。

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// 一组静态凭据（Python 侧 `conf.AWS_PARAMS[scheme]` 里那三件套）。
#[derive(Debug, Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

impl Credentials {
    pub fn new(access_key_id: &str, secret_access_key: &str, session_token: &str) -> Self {
        Self {
            access_key_id: access_key_id.trim().to_string(),
            secret_access_key: secret_access_key.trim().to_string(),
            session_token: match session_token.trim() {
                "" => None,
                t => Some(t.to_string()),
            },
        }
    }
}

/// 一次请求的签名输入。
pub struct SignInput<'a> {
    /// `GET` / `POST`。
    pub method: &'a str,
    /// 例：`bucket.s3.cn-northwest-1.amazonaws.com.cn`。
    pub host: &'a str,
    /// 已经做过 URI 编码的路径（`/` 保留不编码），以 `/` 开头。
    pub canonical_uri: &'a str,
    /// 已经「按名+值排序并编码」的查询串，没有查询参数时传空串。
    pub canonical_query: &'a str,
    /// 除 host / 时间戳 / 会话令牌外、需要参与签名的头（名不限大小写）。
    pub headers: Vec<(String, String)>,
    /// 请求体的 SHA256（hex）。GET 无 body 时用空串的 SHA256。
    pub payload_sha256_hex: &'a str,
}

/// 空 body 的 SHA256，GET 请求用。
pub const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// 签名结果：需要补到实际 HTTP 请求上的头。
pub struct Signed {
    /// `(头名, 值)`，直接塞进 reqwest 的请求头即可。
    pub headers: Vec<(String, String)>,
}

/// 对请求做 SigV4 签名。
///
/// `region` / `service` 决定签名范围（S3 是 `s3`、KMS 是 `kms`）。
pub fn sign_request(creds: &Credentials, region: &str, service: &str, input: &SignInput) -> Signed {
    let (amz_date, date_stamp) = utc_now_stamps();

    // 1) 参与签名的头：必须包含 host 与 x-amz-date，外加调用方给的头。
    let mut to_sign: Vec<(String, String)> = Vec::with_capacity(input.headers.len() + 3);
    to_sign.push(("host".to_string(), input.host.to_string()));
    to_sign.push(("x-amz-date".to_string(), amz_date.clone()));
    for (k, v) in &input.headers {
        to_sign.push((k.to_ascii_lowercase(), v.clone()));
    }
    if let Some(t) = &creds.session_token {
        to_sign.push(("x-amz-security-token".to_string(), t.clone()));
    }
    to_sign.sort_by(|a, b| a.0.cmp(&b.0));

    let canonical_headers: String = to_sign
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", v.trim()))
        .collect();
    let signed_headers = to_sign
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");

    // 2) 规范请求。
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        input.method, input.canonical_uri, input.canonical_query, canonical_headers, signed_headers, input.payload_sha256_hex
    );

    // 3) 待签字符串 + 签名密钥。
    let scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let signing_key = signing_key(&creds.secret_access_key, &date_stamp, region, service);
    let signature = hex(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    // 4) 组装 Authorization 头。
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        creds.access_key_id
    );

    let mut out = vec![
        ("x-amz-date".to_string(), amz_date),
        ("authorization".to_string(), authorization),
    ];
    if let Some(t) = &creds.session_token {
        out.push(("x-amz-security-token".to_string(), t.clone()));
    }
    Signed { headers: out }
}

/// 计算 KMS 等接口需要的 `X-Amz-Target` 之类的常量头，单独放这里方便复用。
pub fn content_sha256(body: &[u8]) -> String {
    sha256_hex(body)
}

/// 把对象 key 转成规范路径（`/` 保留，其余按 RFC3986 编码）。
pub fn canonical_path(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 1);
    for b in key.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    if !out.starts_with('/') {
        out.insert(0, '/');
    }
    out
}

/// `YYYYMMDDTHHMMSSZ` 与 `YYYYMMDD`（UTC）。
pub fn utc_now_stamps() -> (String, String) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    (
        format!("{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}Z"),
        format!("{y:04}{m:02}{d:02}"),
    )
}

/// 把「1970-01-01 起的天数」换成公历日期（Howard Hinnant 的算法）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn signing_key(secret: &str, date_stamp: &str, region: &str, service: &str) -> [u8; 32] {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    mac.update(data);
    let out = mac.finalize().into_bytes();
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&out);
    buf
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// base64 解码（标准字母表）。S3 客户端加密的 metadata 都是标准 base64（可能带填充）。
pub fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = s.trim().chars().filter(|c| !c.is_whitespace()).collect();
    B64.decode(cleaned.as_bytes())
        .map_err(|e| format!("base64 解码失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }

    #[test]
    fn path_encoding() {
        assert_eq!(canonical_path("a/b c.docx"), "/a/b%20c.docx");
        assert_eq!(canonical_path("/x/y"), "/x/y");
    }
}
