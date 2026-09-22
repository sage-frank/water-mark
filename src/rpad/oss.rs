//! 按 scheme 从 S3 取对象，并完成客户端解密。
//!
//! 对应 Python 的 `rpa/utility/HHOss.py`（`OssGeneral` / `OssMFront` / `OssCrm` /
//! `OssCrmV2` / `OssApp`）和 `utility/cpython_crm.py`（调用 Go 编译的
//! `kms_x64.dll`）。
//!
//! 五种 scheme 的差别只在「怎么把密文/明文变成明文」：
//!
//! | scheme   | 取对象 | 解密                                                        |
//! |----------|--------|-------------------------------------------------------------|
//! | mfront   | GET    | metadata `x-amz-key-v2` 过 KMS → AES-GCM（tag 在末尾 16 字节） |
//! | crm      | GET    | 同上，KMS 的 EncryptionContext 多一个 `context-key`           |
//! | crm_v2   | GET    | 优先调 `kms_x64.dll`（与 Python 一致），没配 DLL 时走原生解密   |
//! | hhcrm    | GET    | metadata `k` 过 KMS → 32 字节密钥 → Fernet                    |
//! | other    | GET    | 不做任何处理                                                  |

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;

use super::config::{OssConfig, SchemeConfig};
use super::crypto;
use super::sigv4::{self, Credentials, EMPTY_PAYLOAD_SHA256, SignInput};

/// Python `api.py` 里的 scheme 取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Mfront,
    Crm,
    CrmV2,
    Hhcrm,
    Other,
}

impl Scheme {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "mfront" => Some(Self::Mfront),
            "crm" => Some(Self::Crm),
            "crm_v2" => Some(Self::CrmV2),
            "hhcrm" => Some(Self::Hhcrm),
            "other" => Some(Self::Other),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mfront => "mfront",
            Self::Crm => "crm",
            Self::CrmV2 => "crm_v2",
            Self::Hhcrm => "hhcrm",
            Self::Other => "other",
        }
    }
}

#[derive(Debug)]
pub enum OssError {
    /// S3 上没这个对象（Python 侧对应「文件不存在」分支）。
    NotFound,
    /// 鉴权失败（403/401）。
    Denied(String),
    /// 网络或非预期状态码。
    Http(String),
    /// 解密失败。
    Decrypt(String),
    /// Go DLL 相关失败。
    Dll(String),
    /// 配置缺失。
    Config(String),
}

impl fmt::Display for OssError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "S3 对象不存在"),
            Self::Denied(m) => write!(f, "S3/KMS 鉴权失败: {m}"),
            Self::Http(m) => write!(f, "S3/KMS 请求失败: {m}"),
            Self::Decrypt(m) => write!(f, "解密失败: {m}"),
            Self::Dll(m) => write!(f, "kms 动态库调用失败: {m}"),
            Self::Config(m) => write!(f, "配置问题: {m}"),
        }
    }
}

impl std::error::Error for OssError {}

pub struct OssClient {
    scheme: Scheme,
    cfg: SchemeConfig,
    creds: Credentials,
    http: reqwest::Client,
    tmp_dir: PathBuf,
    kms_dll: Option<PathBuf>,
}

impl OssClient {
    pub fn new(scheme: Scheme, cfg: &SchemeConfig, oss: &OssConfig, tmp_dir: PathBuf) -> Result<Self, OssError> {
        if cfg.bucket_id.trim().is_empty() {
            return Err(OssError::Config(format!("scheme.{} 缺 bucket_id", scheme.as_str())));
        }
        let timeout = Duration::from_millis(oss.http_timeout_ms.unwrap_or(60_000));
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| OssError::Http(format!("初始化 HTTP 客户端失败: {e}")))?;
        let kms_dll = match oss.kms_dll_path.trim() {
            "" => None,
            p => Some(PathBuf::from(p)),
        };
        Ok(Self {
            scheme,
            cfg: cfg.clone(),
            creds: Credentials::new(
                &cfg.aws_access_key_id,
                &cfg.aws_secret_access_key,
                &cfg.session_token,
            ),
            http,
            tmp_dir,
            kms_dll,
        })
    }

    pub fn bucket(&self) -> &str {
        self.cfg.bucket_id.trim()
    }

    /// 取对象并按 scheme 解密，返回明文。
    pub async fn get(&self, key: &str) -> Result<Vec<u8>, OssError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(OssError::Config("对象 key 为空".to_string()));
        }

        // crm_v2 走 Go DLL 时是「DLL 自己下载 + 解密 + 落盘」，和 Python 一模一样。
        if self.scheme == Scheme::CrmV2 && self.kms_dll.is_some() {
            return self.get_via_dll(key).await;
        }

        let (body, meta) = self.get_object(key).await?;
        self.decrypt(&body, &meta).await
    }

    // ------------------------------------------------------------------
    // S3 / KMS
    // ------------------------------------------------------------------

    /// GET 对象，返回 body 和用户 metadata（键已去掉 `x-amz-meta-` 前缀并转小写）。
    async fn get_object(&self, key: &str) -> Result<(Vec<u8>, BTreeMap<String, String>), OssError> {
        let bucket = self.bucket();
        let (origin, path_style) = self.s3_origin();
        let canonical_uri = if path_style {
            sigv4::canonical_path(&format!("{bucket}/{key}"))
        } else {
            sigv4::canonical_path(key)
        };
        let url = format!("{origin}{canonical_uri}");

        let host = url_host(&url)?;
        let signed = sigv4::sign_request(
            &self.creds,
            self.region(),
            "s3",
            &SignInput {
                method: "GET",
                host: &host,
                canonical_uri: &canonical_uri,
                canonical_query: "",
                headers: vec![(
                    "x-amz-content-sha256".to_string(),
                    EMPTY_PAYLOAD_SHA256.to_string(),
                )],
                payload_sha256_hex: EMPTY_PAYLOAD_SHA256,
            },
        );

        let mut req = self
            .http
            .get(&url)
            .header("x-amz-content-sha256", EMPTY_PAYLOAD_SHA256);
        for (k, v) in &signed.headers {
            req = req.header(k, v);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| OssError::Http(format!("GET {url} 失败: {e}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(OssError::NotFound);
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let msg = format!("GET {url} 返回 {status}: {}", truncate(&text, 400));
            return Err(if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::UNAUTHORIZED {
                OssError::Denied(msg)
            } else {
                OssError::Http(msg)
            });
        }

        // 用户 metadata：`x-amz-meta-x-amz-key-v2` → `x-amz-key-v2`。
        let mut meta = BTreeMap::new();
        for (name, value) in resp.headers().iter() {
            let name = name.as_str().to_ascii_lowercase();
            if let Some(short) = name.strip_prefix("x-amz-meta-") {
                if let Ok(v) = value.to_str() {
                    meta.insert(short.to_string(), v.to_string());
                }
            }
        }

        let body = resp
            .bytes()
            .await
            .map_err(|e| OssError::Http(format!("读取 {url} 响应体失败: {e}")))?;
        Ok((body.to_vec(), meta))
    }

    /// KMS Decrypt。
    ///
    /// `context` 为 None 时等价于 Python 里不传 `EncryptionContext`。
    async fn kms_decrypt(
        &self,
        ciphertext: &[u8],
        context: Option<&BTreeMap<String, String>>,
    ) -> Result<Vec<u8>, OssError> {
        let endpoint = self.kms_origin();
        let body_value = match context {
            Some(ctx) => serde_json::json!({
                "CiphertextBlob": B64.encode(ciphertext),
                "EncryptionContext": ctx,
            }),
            None => serde_json::json!({ "CiphertextBlob": B64.encode(ciphertext) }),
        };
        let body = serde_json::to_vec(&body_value)
            .map_err(|e| OssError::Http(format!("构造 KMS 请求失败: {e}")))?;

        let host = url_host(&endpoint)?;
        let payload_hash = sigv4::content_sha256(&body);
        let signed = sigv4::sign_request(
            &self.creds,
            self.region(),
            "kms",
            &SignInput {
                method: "POST",
                host: &host,
                canonical_uri: "/",
                canonical_query: "",
                headers: vec![
                    (
                        "content-type".to_string(),
                        "application/x-amz-json-1.1".to_string(),
                    ),
                    ("x-amz-target".to_string(), "TrentService.Decrypt".to_string()),
                ],
                payload_sha256_hex: &payload_hash,
            },
        );

        let mut req = self
            .http
            .post(&endpoint)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", "TrentService.Decrypt")
            .body(body);
        for (k, v) in &signed.headers {
            req = req.header(k, v);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| OssError::Http(format!("KMS Decrypt 请求失败: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| OssError::Http(format!("读取 KMS 响应失败: {e}")))?;
        if !status.is_success() {
            let msg = format!("KMS Decrypt 返回 {status}: {}", truncate(&text, 400));
            return Err(if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::UNAUTHORIZED {
                OssError::Denied(msg)
            } else {
                OssError::Http(msg)
            });
        }
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| OssError::Http(format!("KMS 响应不是 JSON: {e}: {}", truncate(&text, 200))))?;
        let plaintext = parsed
            .get("Plaintext")
            .and_then(|v| v.as_str())
            .ok_or_else(|| OssError::Http(format!("KMS 响应里没有 Plaintext: {}", truncate(&text, 200))))?;
        B64.decode(plaintext.trim().as_bytes())
            .map_err(|e| OssError::Http(format!("KMS Plaintext base64 解码失败: {e}")))
    }

    // ------------------------------------------------------------------
    // 解密分支
    // ------------------------------------------------------------------

    async fn decrypt(
        &self,
        body: &[u8],
        meta: &BTreeMap<String, String>,
    ) -> Result<Vec<u8>, OssError> {
        match self.scheme {
            Scheme::Other => Ok(body.to_vec()),
            Scheme::Mfront | Scheme::Crm | Scheme::CrmV2 => {
                self.decrypt_s3crypto_gcm(body, meta, self.scheme).await
            }
            Scheme::Hhcrm => self.decrypt_fernet(body, meta).await,
        }
    }

    /// 标准 s3crypto(AES-GCM) 解密：`x-amz-key-v2` / `x-amz-iv` / `x-amz-matdesc`。
    async fn decrypt_s3crypto_gcm(
        &self,
        body: &[u8],
        meta: &BTreeMap<String, String>,
        scheme: Scheme,
    ) -> Result<Vec<u8>, OssError> {
        if body.len() <= 16 {
            return Err(OssError::Decrypt(format!(
                "密文长度 {} 不足（AES-GCM 至少 17 字节）",
                body.len()
            )));
        }
        let key_enc_b64 = meta
            .get("x-amz-key-v2")
            .or_else(|| meta.get("x-amz-key"))
            .ok_or_else(|| OssError::Decrypt("缺少 metadata: x-amz-key-v2".to_string()))?;
        let iv_b64 = meta
            .get("x-amz-iv")
            .ok_or_else(|| OssError::Decrypt("缺少 metadata: x-amz-iv".to_string()))?;

        let wrapped_key = sigv4::b64_decode(key_enc_b64).map_err(OssError::Decrypt)?;
        let iv = sigv4::b64_decode(iv_b64).map_err(OssError::Decrypt)?;

        let ctx = self.encryption_context(meta, scheme);
        let data_key = self.kms_decrypt(&wrapped_key, ctx.as_ref()).await?;

        let (ct, tag) = body.split_at(body.len() - 16);
        crypto::aes_gcm_decrypt(&data_key, &iv, ct, tag).map_err(OssError::Decrypt)
    }

    /// KMS 的 EncryptionContext：
    /// 优先用对象自带的 `x-amz-matdesc`（s3crypto 把加密时的 context 存在这里），
    /// 没有时才退回 Python 里硬编码的那份。
    fn encryption_context(
        &self,
        meta: &BTreeMap<String, String>,
        scheme: Scheme,
    ) -> Option<BTreeMap<String, String>> {
        if let Some(matdesc) = meta.get("x-amz-matdesc") {
            if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(matdesc) {
                let mut ctx = BTreeMap::new();
                for (k, v) in map {
                    if let Some(v) = v.as_str() {
                        ctx.insert(k, v.to_string());
                    }
                }
                if !ctx.is_empty() {
                    return Some(ctx);
                }
            }
        }
        // 与 Python 保持一致：
        //   mfront: {'aws:x-amz-cek-alg': 'AES/GCM/NoPadding'}
        //   crm   : {'aws:x-amz-cek-alg': 'AES/GCM/NoPadding', 'context-key': 'context-value'}
        let mut ctx = BTreeMap::new();
        ctx.insert(
            "aws:x-amz-cek-alg".to_string(),
            "AES/GCM/NoPadding".to_string(),
        );
        if scheme == Scheme::Crm {
            ctx.insert("context-key".to_string(), "context-value".to_string());
        }
        if scheme == Scheme::Mfront || scheme == Scheme::Crm {
            Some(ctx)
        } else {
            // crm_v2 走的 Go SDK（RegisterKMSContextWrapWithAnyCMK）默认不带 context。
            None
        }
    }

    /// `hhcrm`：metadata `k` 过 KMS → 32 字节密钥 → Fernet 解整个 body。
    async fn decrypt_fernet(
        &self,
        body: &[u8],
        meta: &BTreeMap<String, String>,
    ) -> Result<Vec<u8>, OssError> {
        let k_b64 = meta
            .get("k")
            .ok_or_else(|| OssError::Decrypt("缺少 metadata: k".to_string()))?;
        let wrapped = sigv4::b64_decode(k_b64).map_err(OssError::Decrypt)?;
        // Python 侧拿到 Plaintext 后是 `base64.b64encode(...)` 再交给 Fernet，
        // 而 Fernet 内部又 base64 解回来，所以这里直接用 32 字节原始密钥。
        let raw_key = self.kms_decrypt(&wrapped, None).await?;
        crypto::fernet_decrypt(&raw_key, body).map_err(OssError::Decrypt)
    }

    // ------------------------------------------------------------------
    // crm_v2：调 Go 编译的 kms_x64.dll（与 Python 完全一致）
    // ------------------------------------------------------------------

    async fn get_via_dll(&self, key: &str) -> Result<Vec<u8>, OssError> {
        let dll = self.kms_dll.clone().expect("调用前已判断非空");
        let bucket = self.bucket().to_string();
        let region = self.region().to_string();
        let ak = self.cfg.aws_access_key_id.clone();
        let sk = self.cfg.aws_secret_access_key.clone();
        let token = self.cfg.session_token.clone();
        let tmp_dir = self.tmp_dir.clone();
        let key_owned = key.to_string();

        // 动态库是阻塞调用且内部会写文件，放到阻塞线程池里跑。
        let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, OssError> {
            let dir = tempfile::Builder::new()
                .prefix("rpad-crmv2-")
                .tempdir_in(&tmp_dir)
                .map_err(|e| OssError::Dll(format!("创建临时目录失败（{}）: {e}", tmp_dir.display())))?;
            let out_path = dir.path().join("object.bin");

            unsafe {
                let lib = libloading::Library::new(&dll)
                    .map_err(|e| OssError::Dll(format!("加载 {} 失败: {e}", dll.display())))?;

                // Go 侧导出：Init(region, id, key, token) -> int
                {
                    let init: libloading::Symbol<unsafe extern "C" fn(*const i8, *const i8, *const i8, *const i8) -> i32> =
                        lib.get(b"Init")
                            .map_err(|e| OssError::Dll(format!("找不到导出函数 Init: {e}")))?;
                    let region = cstr(&region)?;
                    let ak = cstr(&ak)?;
                    let sk = cstr(&sk)?;
                    let token = cstr(&token)?;
                    let rc = init(
                        region.as_ptr() as *const i8,
                        ak.as_ptr() as *const i8,
                        sk.as_ptr() as *const i8,
                        token.as_ptr() as *const i8,
                    );
                    if rc != 0 {
                        return Err(OssError::Dll(format!("Init 返回 {rc}")));
                    }
                }

                // GetObjectV2(bucket, key, outPath) -> int64（Go 侧导出的是 int64，
                // 成功返回 200、失败 -1；与 Python 侧 ctypes 声明一致）
                {
                    let get: libloading::Symbol<unsafe extern "C" fn(*const i8, *const i8, *const i8) -> i64> =
                        lib.get(b"GetObjectV2")
                            .map_err(|e| OssError::Dll(format!("找不到导出函数 GetObjectV2: {e}")))?;
                    let bucket = cstr(&bucket)?;
                    let key = cstr(&key_owned)?;
                    let out = cstr(&out_path.to_string_lossy())?;
                    let rc = get(
                        bucket.as_ptr() as *const i8,
                        key.as_ptr() as *const i8,
                        out.as_ptr() as *const i8,
                    );
                    if rc != 200 {
                        return Err(OssError::Dll(format!("GetObjectV2 返回 {rc}")));
                    }
                }
            }

            // Python 侧是写完文件后再检查文件是否存在，这里等价处理。
            let data = std::fs::read(&out_path).map_err(|e| {
                OssError::Dll(format!("动态库没有生成文件 {}: {e}", out_path.display()))
            })?;
            if data.is_empty() {
                return Err(OssError::NotFound);
            }
            Ok(data)
        })
        .await
        .map_err(|e| OssError::Dll(format!("调用动态库的任务 panic: {e}")))??;

        Ok(bytes)
    }

    // ------------------------------------------------------------------
    // 端点推导
    // ------------------------------------------------------------------

    /// 返回 `(origin, path_style)`。origin 形如 `https://bucket.s3.cn-northwest-1.amazonaws.com.cn`。
    fn s3_origin(&self) -> (String, bool) {
        let bucket = self.bucket();
        let path_style = self.cfg.force_path_style || bucket.contains('.');
        if !self.cfg.endpoint.trim().is_empty() {
            let base = self.cfg.endpoint.trim().trim_end_matches('/');
            return if path_style {
                (base.to_string(), true)
            } else {
                (inject_bucket(base, bucket), false)
            };
        }
        let host = s3_host(self.region());
        if path_style {
            (format!("https://{host}"), true)
        } else {
            (format!("https://{bucket}.{host}"), false)
        }
    }

    fn kms_origin(&self) -> String {
        if !self.cfg.kms_endpoint.trim().is_empty() {
            return format!("{}/", self.cfg.kms_endpoint.trim().trim_end_matches('/'));
        }
        let region = self.region();
        let host = if region.is_empty() {
            "kms.amazonaws.com".to_string()
        } else if region.starts_with("cn-") {
            format!("kms.{region}.amazonaws.com.cn")
        } else {
            format!("kms.{region}.amazonaws.com")
        };
        format!("https://{host}/")
    }

    fn region(&self) -> &str {
        self.cfg.region_name.trim()
    }
}

/// `https://host` → `https://bucket.host`；已经带桶名的端点原样返回。
fn inject_bucket(base: &str, bucket: &str) -> String {
    match base.split_once("://") {
        Some((scheme, rest)) => {
            let (host, tail) = match rest.split_once('/') {
                Some((h, t)) => (h, format!("/{t}")),
                None => (rest, String::new()),
            };
            if host.starts_with(&format!("{bucket}.")) {
                base.to_string()
            } else {
                format!("{scheme}://{bucket}.{host}{tail}")
            }
        }
        None => base.to_string(),
    }
}

fn s3_host(region: &str) -> String {
    if region.is_empty() {
        "s3.amazonaws.com".to_string()
    } else if region.starts_with("cn-") {
        format!("s3.{region}.amazonaws.com.cn")
    } else {
        format!("s3.{region}.amazonaws.com")
    }
}

/// 取 URL 里参与签名的 host（默认端口不带端口号，与 reqwest 实际发出的 Host 头一致）。
fn url_host(url: &str) -> Result<String, OssError> {
    let parsed = url::Url::parse(url).map_err(|e| OssError::Http(format!("URL 非法 {url}: {e}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| OssError::Http(format!("URL 没有 host: {url}")))?;
    match parsed.port() {
        Some(p) if Some(p) != parsed.port_or_known_default() => Ok(format!("{host}:{p}")),
        _ => Ok(host.to_string()),
    }
}

fn cstr(s: &str) -> Result<CString, OssError> {
    CString::new(s).map_err(|_| OssError::Dll(format!("字符串里含 NUL 字节: {s:?}")))
}

fn truncate(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        let cut: String = t.chars().take(max).collect();
        format!("{cut}…")
    }
}
