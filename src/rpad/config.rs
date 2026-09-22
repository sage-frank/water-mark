//! rpad 的配置。
//!
//! 对应 Python 侧的 `conf.py`：`AWS_PARAMS` 是「按 scheme 分组的 S3/KMS 凭据 +
//! bucket」，`template_file_path_window` / `template_file_path_linux` 是临时目录。
//!
//! 这里用 TOML 文件（默认 `rpad.toml`，可用 `RPAD_CONFIG` 指定）+ 少量环境变量
//! 覆盖。凭据不写死在代码里，也不内置默认值：配置文件缺失/字段缺失一律启动即报错，
//! 而不是等到第一个请求才 500。
//!
//! ```toml
//! [server]
//! addr = "0.0.0.0:8090"          # 对应 Flask 的 0.0.0.0:8090
//! concurrency = 1                # 1 等价于 Python 那把全局锁（完全串行）
//! acquire_timeout_ms = 30000     # 对应 XLock.acquire(acquire_timeout=30)
//!
//! [oss]
//! tmp_dir = "C:\\tmp"            # 对应 conf.template_file_path_window
//! dump_dir = ""                  # 非空则把中间 docx 落盘，便于人工核对
//! kms_dll_path = ""              # crm_v2 的 kms_x64.dll；留空则用原生 s3crypto 解密
//!
//! [scheme.mfront]                # 对应 conf.AWS_PARAMS["mfront"]
//! aws_access_key_id = "..."
//! aws_secret_access_key = "..."
//! region_name = "cn-northwest-1"
//! bucket_id = "..."
//! kms_id = "..."
//! ```

use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// Python `api.py` 里的 `SCHEME = ["mfront", "crm", "crm_v2", "hhcrm", "other"]`。
pub const SCHEMES: [&str; 5] = ["mfront", "crm", "crm_v2", "hhcrm", "other"];

/// 单个 scheme 的 S3/KMS 配置（对应 `conf.AWS_PARAMS[scheme]` 的一项）。
#[derive(Debug, Clone, Deserialize)]
pub struct SchemeConfig {
    pub aws_access_key_id: String,
    pub aws_secret_access_key: String,
    pub bucket_id: String,
    #[serde(default)]
    pub region_name: String,
    /// KMS 主密钥，`mfront` / `crm` / `hhcrm` / `crm_v2` 用得上。
    #[serde(default)]
    pub kms_id: String,
    /// 临时凭据（STS）时用。
    #[serde(default)]
    pub session_token: String,
    /// 自定义 S3 端点（如内网 MinIO/代理）；留空则按 region 推导 AWS 域名。
    #[serde(default)]
    pub endpoint: String,
    /// 自定义 KMS 端点；留空按 region 推导。
    #[serde(default)]
    pub kms_endpoint: String,
    /// 强制 path-style（`https://host/bucket/key`）；默认桶名做子域。
    #[serde(default)]
    pub force_path_style: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_addr")]
    pub addr: String,
    /// 并发闸门。默认 1：与 Python 的全局锁等价（那边是为了迁就 Word COM）。
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// 拿不到并发许可的等待上限，超时按 Python 的「抢锁失败」处理（409）。
    #[serde(default = "default_acquire_timeout_ms")]
    pub acquire_timeout_ms: u64,
    /// 单请求整体超时（取 S3 + 拼接 + 渲染），0 表示不限。
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: default_addr(),
            concurrency: default_concurrency(),
            acquire_timeout_ms: default_acquire_timeout_ms(),
            request_timeout_ms: default_request_timeout_ms(),
        }
    }
}

fn default_addr() -> String {
    "0.0.0.0:8090".to_string()
}

/// 1 = 与 Python 完全一致的串行行为；想压测/提速可调大。
fn default_concurrency() -> usize {
    1
}

fn default_acquire_timeout_ms() -> u64 {
    30_000
}

fn default_request_timeout_ms() -> u64 {
    // Python 的锁超时是 240s，转换期间不会被打断；这里给一个更宽松的上限。
    300_000
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OssConfig {
    /// 临时目录：Python 会先把 S3 对象落盘再处理。
    /// Rust 侧默认全内存，只有「crm_v2 走 Go DLL」这一条路必须落盘。
    #[serde(default)]
    pub tmp_dir: String,
    /// 非空则把「填完占位符的模板」和「拼接后的 docx」按请求落盘一份，便于人工核对。
    #[serde(default)]
    pub dump_dir: String,
    /// Go 编译的 `kms_x64.dll` / `.so` 路径。留空时 `crm_v2` 走 Rust 原生解密。
    #[serde(default)]
    pub kms_dll_path: String,
    /// S3/KMS 单次 HTTP 超时。
    #[serde(default)]
    pub http_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub oss: OssConfig,
    /// `[scheme.xxx]` 分组。
    #[serde(default)]
    pub scheme: HashMap<String, SchemeConfig>,
}

/// 运行期配置（文件 + 环境变量合并后的结果）。
#[derive(Debug, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub addr: SocketAddr,
    pub oss: OssConfig,
    pub schemes: HashMap<String, SchemeConfig>,
    /// 配置文件路径（日志里打出来，方便排查「到底读的哪份配置」）。
    pub source: String,
}

impl Config {
    pub fn scheme_config(&self, scheme: &str) -> Result<&SchemeConfig, String> {
        self.schemes
            .get(scheme)
            .ok_or_else(|| format!("配置里没有 scheme.{scheme}（conf. 缺 AWS_PARAMS[{scheme}]）"))
    }

    pub fn tmp_dir(&self) -> PathBuf {
        if self.oss.tmp_dir.trim().is_empty() {
            env::temp_dir()
        } else {
            PathBuf::from(self.oss.tmp_dir.trim())
        }
    }

    /// 日志文件路径（追加写，`None` = 只走 stdout）。
    ///
    /// - `RPAD_LOG_FILE=<路径>` → 写这个文件；
    /// - 未设置 → 默认 `<tmp_dir>/rpad.log`。默认落盘是有意的：以服务方式运行时
    ///   stdout 经常被丢弃，release 版会表现为「完全没有日志输出」；
    /// - `RPAD_LOG_FILE=off` / `none` / `stdout` → 不落盘，只走 stdout。
    pub fn log_file(&self) -> Option<PathBuf> {
        match env::var("RPAD_LOG_FILE") {
            Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
                "" | "off" | "none" | "stdout" => None,
                p => Some(PathBuf::from(p)),
            },
            Err(_) => Some(self.tmp_dir().join("rpad.log")),
        }
    }

    /// 占位符严格模式，**默认关闭**：与 Python `docxtpl` 一致，模板里有取不到值的
    /// 占位符时渲染成空串（并记日志），而不是让请求失败。
    ///
    /// 想恢复「有未知占位符就失败」的行为，设 `RPAD_STRICT_PLACEHOLDERS=1`。
    pub fn strict_placeholders(&self) -> bool {
        env_flag("RPAD_STRICT_PLACEHOLDERS")
    }

    pub fn dump_dir(&self) -> Option<PathBuf> {
        let d = self.oss.dump_dir.trim();
        if d.is_empty() {
            None
        } else {
            Some(PathBuf::from(d))
        }
    }

    pub fn acquire_timeout(&self) -> Duration {
        Duration::from_millis(self.server.acquire_timeout_ms.max(1))
    }

    pub fn request_timeout(&self) -> Option<Duration> {
        match self.server.request_timeout_ms {
            0 => None,
            ms => Some(Duration::from_millis(ms)),
        }
    }

    /// 从文件 + 环境变量加载。
    ///
    /// `path` 为 `None` 时按顺序找：`RPAD_CONFIG` → `./rpad.toml`。
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let file_path = match path {
            Some(p) => Some(p.to_path_buf()),
            None => match env::var("RPAD_CONFIG") {
                Ok(p) if !p.trim().is_empty() => Some(PathBuf::from(p.trim())),
                _ => {
                    let d = PathBuf::from("rpad.toml");
                    if d.exists() { Some(d) } else { None }
                }
            },
        };

        let mut cfg = match &file_path {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .map_err(|e| format!("读取配置文件 {} 失败: {e}", p.display()))?;
                let mut c: FileConfig = toml::from_str(&text)
                    .map_err(|e| format!("解析配置文件 {} 失败: {e}", p.display()))?;
                // 空字符串一律当作「没配」，免得 `region_name = ""` 把推导逻辑带偏。
                for s in c.scheme.values_mut() {
                    s.region_name = s.region_name.trim().to_string();
                    s.session_token = s.session_token.trim().to_string();
                    s.endpoint = s.endpoint.trim().trim_end_matches('/').to_string();
                    s.kms_endpoint = s.kms_endpoint.trim().trim_end_matches('/').to_string();
                }
                Config {
                    server: c.server.clone(),
                    addr: parse_addr(&c.server.addr)?,
                    oss: c.oss.clone(),
                    schemes: c.scheme.clone(),
                    source: p.display().to_string(),
                }
            }
            None => Config {
                addr: parse_addr(&default_addr())?,
                server: ServerConfig::default(),
                oss: OssConfig::default(),
                schemes: HashMap::new(),
                source: "(无配置文件，仅环境变量)".to_string(),
            },
        };

        // `RPAD_SCHEMES` 直接给一整段 JSON，容器部署时最省事。
        if let Ok(json) = env::var("RPAD_SCHEMES") {
            let trimmed = json.trim();
            if !trimmed.is_empty() {
                let extra: HashMap<String, SchemeConfig> = serde_json::from_str(trimmed)
                    .map_err(|e| format!("RPAD_SCHEMES 不是合法的 JSON 配置: {e}"))?;
                for (k, v) in extra {
                    cfg.schemes.insert(k, v);
                }
                cfg.source = format!("{} + RPAD_SCHEMES", cfg.source);
            }
        }

        // 少量环境变量覆盖，便于容器里调参而不用改配置。
        if let Some(v) = env_str("RPAD_ADDR") {
            cfg.server.addr = v.clone();
            cfg.addr = parse_addr(&v)?;
        }
        if let Some(v) = env_usize("RPAD_CONCURRENCY") {
            cfg.server.concurrency = v;
        }
        if let Some(v) = env_usize("RPAD_ACQUIRE_TIMEOUT_MS") {
            cfg.server.acquire_timeout_ms = v as u64;
        }
        if let Some(v) = env_usize("RPAD_REQUEST_TIMEOUT_MS") {
            cfg.server.request_timeout_ms = v as u64;
        }
        if let Some(v) = env_str("RPAD_TMP_DIR") {
            cfg.oss.tmp_dir = v;
        }
        if let Some(v) = env_str("RPAD_DUMP_DIR") {
            cfg.oss.dump_dir = v;
        }
        if let Some(v) = env_str("RPAD_KMS_DLL") {
            cfg.oss.kms_dll_path = v;
        }
        if cfg.server.concurrency == 0 {
            cfg.server.concurrency = 1;
        }

        Ok(cfg)
    }

    /// 启动时校验：Python 支持 5 种 scheme，配置里缺哪个就报出来（不阻塞启动，
    /// 因为实际可能只用其中一两种）。
    pub fn missing_schemes(&self) -> Vec<&'static str> {
        SCHEMES
            .iter()
            .copied()
            .filter(|s| !self.schemes.contains_key(*s))
            .collect()
    }

    /// 对所有已配置的 scheme 做一次最基本的体检，尽早暴露「密钥写错/漏字段」。
    pub fn validate_schemes(&self) -> Result<(), String> {
        for (name, s) in &self.schemes {
            if s.aws_access_key_id.trim().is_empty() {
                return Err(format!("scheme.{name}: aws_access_key_id 为空"));
            }
            if s.aws_secret_access_key.trim().is_empty() {
                return Err(format!("scheme.{name}: aws_secret_access_key 为空"));
            }
            if s.bucket_id.trim().is_empty() {
                return Err(format!("scheme.{name}: bucket_id 为空"));
            }
            // mfront/crm/crm_v2 需要 KMS 数据密钥；hhcrm 也需要（metadata 里的 k）。
            if !matches!(name.as_str(), "other")
                && s.kms_id.trim().is_empty()
                && s.region_name.is_empty() {
                return Err(format!(
                    "scheme.{name}: region_name 与 kms_id 不能都为空（至少要能推导出 KMS 端点）"
                ));
            }
        }
        Ok(())
    }
}

fn parse_addr(raw: &str) -> Result<SocketAddr, String> {
    raw.trim()
        .parse()
        .map_err(|e| format!("RPAD_ADDR/配置里的 addr 不是合法监听地址 {raw:?}: {e}"))
}

fn env_str(key: &str) -> Option<String> {
    env::var(key).ok().filter(|s| !s.trim().is_empty())
}

fn env_usize(key: &str) -> Option<usize> {
    env::var(key).ok()?.trim().parse().ok().filter(|v| *v > 0)
}

/// `1` / `true` / `yes` / `on`（忽略大小写）为真，其余（含未设置）为假。
fn env_flag(key: &str) -> bool {
    env::var(key)
        .ok()
        .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}
