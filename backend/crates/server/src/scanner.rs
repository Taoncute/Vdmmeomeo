//! Quét tệp tự động trước khi đưa vào hàng chờ duyệt thủ công:
//! 1) Quét chuỗi đáng ngờ (đánh cắp cookie/token, exfiltration webhook, obfuscate nặng).
//! 2) Quét virus qua ClamAV (`clamd` giao thức INSTREAM) nếu `CLAMD_ADDR` được cấu hình.

use async_trait::async_trait;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::AppResult;

#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    /// `clean` | `suspicious` | `infected` | `error`
    pub status: String,
    pub clamav_checked: bool,
    pub clamav_result: Option<String>,
    pub suspicious_findings: Vec<String>,
}

#[async_trait]
pub trait FileScanner: Send + Sync {
    async fn scan(&self, filename: &str, data: &[u8]) -> AppResult<ScanReport>;
}

const SUSPICIOUS_PATTERNS: &[(&str, &str)] = &[
    (
        "document.cookie",
        "Đọc cookie trình duyệt (document.cookie)",
    ),
    (
        "discord.com/api/webhooks",
        "Gửi dữ liệu qua Discord webhook",
    ),
    (
        "discordapp.com/api/webhooks",
        "Gửi dữ liệu qua Discord webhook",
    ),
    ("powershell -enc", "Chạy PowerShell mã hóa lệnh (-enc)"),
    ("powershell -encodedcommand", "Chạy PowerShell mã hóa lệnh"),
    ("eval(atob(", "Obfuscate/thực thi mã động eval(atob)"),
    (
        "eval(base64_decode",
        "Obfuscate/thực thi mã động eval(base64_decode)",
    ),
    (
        "exec(base64.b64decode",
        "Obfuscate/thực thi mã động Python base64",
    ),
    (
        "login data",
        "Truy cập tệp mật khẩu trình duyệt (Login Data)",
    ),
    (
        "local state",
        "Truy cập khóa giải mã trình duyệt (Local State)",
    ),
    ("/dev/tcp/", "Kết nối reverse shell (/dev/tcp/)"),
    ("keylogger", "Từ khóa ghi phím (keylogger)"),
];

/// Quét tĩnh chuỗi đáng ngờ trên nội dung văn bản hoặc nhị phân (lossy UTF-8, không phân biệt hoa thường).
pub fn scan_suspicious_strings(data: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(data).to_ascii_lowercase();
    let mut findings = Vec::new();
    for (needle, label) in SUSPICIOUS_PATTERNS {
        if text.contains(needle) {
            findings.push((*label).to_string());
        }
    }
    findings
}

/// Quét qua `clamd` bằng lệnh `zINSTREAM\0`.
async fn scan_clamd(addr: &str, data: &[u8]) -> Result<String, String> {
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|e| format!("không kết nối được clamd ({addr}): {e}"))?;
    stream
        .write_all(b"zINSTREAM\0")
        .await
        .map_err(|e| format!("lỗi gửi lệnh clamd: {e}"))?;
    for chunk in data.chunks(32 * 1024) {
        let len = (chunk.len() as u32).to_be_bytes();
        stream
            .write_all(&len)
            .await
            .map_err(|e| format!("lỗi gửi kích thước chunk: {e}"))?;
        stream
            .write_all(chunk)
            .await
            .map_err(|e| format!("lỗi gửi dữ liệu tới clamd: {e}"))?;
    }
    stream
        .write_all(&0u32.to_be_bytes())
        .await
        .map_err(|e| format!("lỗi kết thúc stream clamd: {e}"))?;
    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .await
        .map_err(|e| format!("lỗi đọc kết quả clamd: {e}"))?;
    let resp = String::from_utf8_lossy(&buf)
        .trim_end_matches('\0')
        .trim()
        .to_string();
    Ok(resp)
}

pub struct DefaultScanner {
    clamd_addr: Option<String>,
}

impl DefaultScanner {
    pub fn new(clamd_addr: String) -> Self {
        let addr = clamd_addr.trim().to_string();
        Self {
            clamd_addr: if addr.is_empty() { None } else { Some(addr) },
        }
    }
}

#[async_trait]
impl FileScanner for DefaultScanner {
    async fn scan(&self, _filename: &str, data: &[u8]) -> AppResult<ScanReport> {
        let suspicious_findings = scan_suspicious_strings(data);
        let mut clamav_checked = false;
        let mut clamav_result = None;
        let mut infected = false;
        let mut scan_error = false;

        if let Some(addr) = &self.clamd_addr {
            clamav_checked = true;
            match scan_clamd(addr, data).await {
                Ok(resp) => {
                    if resp.ends_with("FOUND") {
                        infected = true;
                    } else if !resp.ends_with("OK") {
                        scan_error = true;
                    }
                    clamav_result = Some(resp);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "clamd không phản hồi");
                    scan_error = true;
                    clamav_result = Some(err);
                }
            }
        }

        let status = if infected {
            "infected"
        } else if scan_error {
            "error"
        } else if !suspicious_findings.is_empty() {
            "suspicious"
        } else {
            "clean"
        }
        .to_string();

        Ok(ScanReport {
            status,
            clamav_checked,
            clamav_result,
            suspicious_findings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::scan_suspicious_strings;

    #[test]
    fn phat_hien_chuoi_dang_ngo() {
        let clean = b"print('hello world')\n";
        assert!(scan_suspicious_strings(clean).is_empty());

        let bad = b"const c = document.cookie; fetch('https://discord.com/api/webhooks/123');";
        let findings = scan_suspicious_strings(bad);
        assert_eq!(findings.len(), 2);
    }
}
