//! 应用内更新：检查（GitHub Releases Atom 摘要源 + 稳定/测试双通道）、下载
//! （进度事件、可取消）与安装（便携版直替 exe 并同步改名为新版本号，安装版
//! 静默重跑 NSIS 安装器）。
//!
//! FloePod 以「不联网」为承诺：更新检查只由用户在设置里主动触发，本模块是
//! 全应用唯一的出网路径。检查走 releases.atom 而不是 REST API——匿名 REST
//! 按 IP 限流，在校园网 / 公司网这类共享出口上很容易直接 403。
//!
//! 渠道语义与发布工作流同源：tag 去掉 v 前缀后含 `-` 即 Pre-Release。
//! 稳定通道只接收正式版；测试通道接收 Pre-Release 与正式版中更新的那个。

use base64::Engine as _;
use quick_xml::events::Event as XmlEvent;
use semver::Version;
use serde::Serialize;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

use crate::events::UPDATE_PROGRESS;

const ATOM_URL: &str = "https://github.com/MogroWang/FloePod/releases.atom";
/// 产物下载前缀；tag 带 v 前缀，文件名不带。
const RELEASE_ASSET_PREFIX: &str = "https://github.com/MogroWang/FloePod/releases/download";
/// 发布说明纯文本的截断长度：向导里只做摘要，全文在 Release 页。
const NOTES_MAX_CHARS: usize = 3000;

/// 当前实例的安装形态：决定下载哪种包体、以及安装方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallKind {
    /// 便携版：下载 zip，退出后直替 exe（文件名同步改为新版本号）。
    Portable,
    /// NSIS 安装版：下载安装器，退出后静默重装（沿用上次安装路径原地升级）。
    Installer,
}

fn install_kind() -> InstallKind {
    if crate::paths::is_portable() {
        InstallKind::Portable
    } else {
        InstallKind::Installer
    }
}

/// MSIX 打包的进程由系统注入该环境变量；侧载包受系统管理，无法被 NSIS
/// 安装器原地升级，自动更新必须让位给手动下载。
fn running_in_msix() -> bool {
    std::env::var_os("PACKAGE_FULL_NAME").is_some_and(|value| !value.is_empty())
}

/// 一次可用的应用更新：按当前安装形态选好了包体与下载地址。
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    /// 新版本号（不含 v 前缀），如 2.0.0-beta2。
    pub version: String,
    /// 是否预发布：tag 含后缀即预发布，与发布工作流的判定同源。
    pub prerelease: bool,
    /// 发布说明（HTML 已剥离为纯文本，超长截断）。
    pub notes: String,
    /// GitHub Release 页面链接。
    pub release_url: String,
    /// 按当前安装形态选择的包体文件名。
    pub asset_name: String,
}

/// 下载进度。total 为 0 表示服务器未提供长度（此时只展示已收字节）。
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProgress {
    pub received: u64,
    pub total: u64,
}

/// tag → 语义化版本 + 是否预发布。预发布判定与 CI 发布规则同源：
/// 去掉 v/V 前缀后含 `-` 即 Pre-Release。
fn parse_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.trim_start_matches(['v', 'V'])).ok()
}

/// 渠道 → 是否接受预发布：稳定通道只看正式版，测试通道两者都收。
fn accepts_tag(channel: &str, tag: &str) -> bool {
    channel == "beta" || !tag.contains('-')
}

/// 包体文件名。与 scripts/package-portable.mjs 和 Tauri NSIS bundler 的
/// 产物名规则保持一致（发布资产不改名）。
fn asset_name(kind: InstallKind, version: &str) -> String {
    match kind {
        InstallKind::Portable => format!("FloePod-{version}-win-x64-portable.zip"),
        InstallKind::Installer => format!("FloePod_{version}_x64-setup.exe"),
    }
}

fn http_get(url: &str, timeout: Duration) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("FloePod/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(timeout)
        .build()
        .map_err(|error| format!("网络初始化失败: {error}"))?;
    let response = client
        .get(url)
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|error| format!("无法访问更新源: {error}"))?;
    response
        .text()
        .map_err(|error| format!("读取更新源失败: {error}"))
}

struct FeedEntry {
    tag: String,
    url: String,
    notes_html: String,
}

/// 解析 releases.atom 的 entry 列表：只需要发布页链接（内含 tag）与说明。
fn parse_atom(body: &str) -> Result<Vec<FeedEntry>, String> {
    let mut reader = quick_xml::Reader::from_str(body);
    reader.config_mut().trim_text(true);
    let mut entries = Vec::new();
    let mut in_entry = false;
    let mut in_content = false;
    let mut href: Option<String> = None;
    let mut notes = String::new();
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_event_into(&mut buffer) {
            Ok(XmlEvent::Start(element)) => match element.name().as_ref() {
                b"entry" => {
                    in_entry = true;
                    href = None;
                    notes.clear();
                }
                b"content" if in_entry => in_content = true,
                b"link" if in_entry => {
                    if let Some(value) = link_href(&element)? {
                        href = Some(value);
                    }
                }
                _ => {}
            },
            Ok(XmlEvent::Empty(element)) => {
                if element.name().as_ref() == b"link" && in_entry {
                    if let Some(value) = link_href(&element)? {
                        href = Some(value);
                    }
                }
            }
            Ok(XmlEvent::Text(text)) if in_content => {
                let raw = String::from_utf8_lossy(&text);
                let decoded = quick_xml::escape::unescape(&raw)
                    .map_err(|error| format!("更新源内容转义无效: {error}"))?;
                notes.push_str(&decoded);
            }
            Ok(XmlEvent::End(element)) => match element.name().as_ref() {
                b"entry" => {
                    in_entry = false;
                    if let Some(url) = href.take() {
                        // 发布页形如 .../releases/tag/v2.0.0-beta1，tag 即末段
                        if let Some(tag) = url.split("/releases/tag/").nth(1) {
                            entries.push(FeedEntry {
                                tag: tag.to_string(),
                                url,
                                notes_html: std::mem::take(&mut notes),
                            });
                        }
                    }
                }
                b"content" => in_content = false,
                _ => {}
            },
            Ok(XmlEvent::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("解析更新源失败: {error}")),
        }
    }
    Ok(entries)
}

fn link_href(element: &quick_xml::events::BytesStart<'_>) -> Result<Option<String>, String> {
    let attribute = element
        .try_get_attribute("href")
        .map_err(|error| format!("更新源属性无效: {error}"))?;
    match attribute {
        Some(attribute) => Ok(Some(
            attribute
                .unescape_value()
                .map_err(|error| format!("更新源属性转义无效: {error}"))?
                .into_owned(),
        )),
        None => Ok(None),
    }
}

/// 发布说明 HTML 转纯文本：块级标签转换行、剥掉其余标签、还原常见实体、
/// 压缩空白并按字符截断。
fn html_to_text(html: &str, max_chars: usize) -> String {
    enum Mode {
        Text,
        Tag,
        Entity,
    }
    let mut text = String::with_capacity(html.len());
    let mut mode = Mode::Text;
    let mut buffer = String::new();
    for character in html.chars() {
        match mode {
            Mode::Tag => {
                if character == '>' {
                    let lower = buffer.to_ascii_lowercase();
                    if lower.starts_with("br")
                        || lower.starts_with("/p")
                        || lower.starts_with("p>")
                        || lower.starts_with("p ")
                        || lower.starts_with("li")
                        || lower.starts_with("/li")
                        || lower.starts_with("/h")
                        || lower.starts_with("/div")
                    {
                        text.push('\n');
                    }
                    mode = Mode::Text;
                } else {
                    buffer.push(character);
                }
            }
            Mode::Entity => {
                if character == ';' {
                    match buffer.as_str() {
                        "&amp" => text.push('&'),
                        "&lt" => text.push('<'),
                        "&gt" => text.push('>'),
                        "&quot" => text.push('"'),
                        "&apos" | "&#39" => text.push('\''),
                        "&nbsp" => text.push(' '),
                        // 未知实体原样保留，不吞内容
                        _ => text.push_str(&buffer),
                    }
                    mode = Mode::Text;
                } else if buffer.chars().count() > 12 {
                    // 撑到这么长还没结束的 '&' 不是实体：原样吐回
                    text.push_str(&buffer);
                    text.push(character);
                    mode = Mode::Text;
                } else {
                    buffer.push(character);
                }
            }
            Mode::Text => match character {
                '<' => {
                    buffer.clear();
                    mode = Mode::Tag;
                }
                '&' => {
                    buffer.clear();
                    buffer.push('&');
                    mode = Mode::Entity;
                }
                _ => text.push(character),
            },
        }
    }
    // 压缩行内空白：发布说明只需要可读，不保留原始排版
    let mut collapsed = String::with_capacity(text.len());
    let mut pending_break = false;
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            continue;
        }
        if pending_break {
            collapsed.push('\n');
        }
        collapsed.push_str(&line);
        pending_break = true;
    }
    if collapsed.chars().count() > max_chars {
        let truncated: String = collapsed.chars().take(max_chars).collect();
        return format!("{truncated}…");
    }
    collapsed
}

// ---- 检查更新 ----

pub fn check(channel: &str) -> Result<Option<UpdateInfo>, String> {
    let body = http_get(ATOM_URL, Duration::from_secs(20))?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| format!("当前版本号无效: {error}"))?;
    let mut best: Option<(Version, FeedEntry)> = None;
    for entry in parse_atom(&body)? {
        let Some(version) = parse_tag(&entry.tag) else {
            continue;
        };
        if !accepts_tag(channel, &entry.tag) || version <= current {
            continue;
        }
        if best.as_ref().is_none_or(|(seen, _)| version > *seen) {
            best = Some((version, entry));
        }
    }
    let Some((version, entry)) = best else {
        return Ok(None);
    };
    let clean_version = entry.tag.trim_start_matches(['v', 'V']).to_string();
    let kind = install_kind();
    Ok(Some(UpdateInfo {
        asset_name: asset_name(kind, &clean_version),
        notes: html_to_text(&entry.notes_html, NOTES_MAX_CHARS),
        prerelease: entry.tag.contains('-'),
        release_url: entry.url,
        version: clean_version,
    }))
}

// ---- 下载 ----

struct StagedUpdate {
    path: PathBuf,
    version: String,
    kind: InstallKind,
}

static DOWNLOADING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);
static STAGED: OnceLock<Mutex<Option<StagedUpdate>>> = OnceLock::new();

fn staged() -> &'static Mutex<Option<StagedUpdate>> {
    STAGED.get_or_init(|| Mutex::new(None))
}

/// 取消进行中的下载：正在写入的分块循环会在下一块前停止并清理临时文件。
pub fn cancel_download() {
    CANCEL.store(true, Ordering::SeqCst);
}

pub fn download(app: &AppHandle, version: &str) -> Result<(), String> {
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        return Err("已有下载在进行".into());
    }
    let result = download_inner(app, version);
    DOWNLOADING.store(false, Ordering::SeqCst);
    result
}

fn download_inner(app: &AppHandle, version: &str) -> Result<(), String> {
    CANCEL.store(false, Ordering::SeqCst);
    let version = version.trim().trim_start_matches(['v', 'V']).to_string();
    // 只接受能被解析为语义化版本的版本号：包体 URL 由后端规则拼出，
    // WebView 传不进来任意路径。
    Version::parse(&version).map_err(|_| format!("版本号无效: {version}"))?;
    let kind = install_kind();
    if running_in_msix() {
        return Err("MSIX 侧载包由系统管理，请到 Release 页手动下载新版".into());
    }
    let asset = asset_name(kind, &version);
    let url = format!("{RELEASE_ASSET_PREFIX}/v{version}/{asset}");
    let updates_dir = app.state::<crate::state::AppState>().data_dir.join("updates");
    std::fs::create_dir_all(&updates_dir).map_err(|error| format!("无法准备更新目录: {error}"))?;
    // 更新目录只服务于一次下载会话：清掉旧残留，避免半截 .part 冒充新包
    for entry in std::fs::read_dir(&updates_dir)
        .map_err(|error| format!("无法读取更新目录: {error}"))?
        .flatten()
    {
        if entry.path().is_file() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let part_path = updates_dir.join(format!("{asset}.part"));
    let final_path = updates_dir.join(&asset);
    // 不设整体超时：包体可能上百 MB；连接超时兜住建连故障，读卡死由用户取消兜底
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("FloePod/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| format!("下载初始化失败: {error}"))?;
    let mut response = client
        .get(&url)
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|error| format!("下载请求失败: {error}"))?;
    let total = response.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(&part_path)
        .map_err(|error| format!("无法写入更新目录: {error}"))?;
    let mut received: u64 = 0;
    let mut last_report: u64 = 0;
    const REPORT_STEP: u64 = 256 * 1024;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            drop(file);
            let _ = std::fs::remove_file(&part_path);
            return Err("已取消下载".into());
        }
        let count = response.read(&mut buffer).map_err(|error| {
            let _ = std::fs::remove_file(&part_path);
            format!("下载中断: {error}")
        })?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])
            .map_err(|error| format!("写入更新包失败: {error}"))?;
        received += count as u64;
        if received - last_report >= REPORT_STEP {
            last_report = received;
            let _ = UPDATE_PROGRESS.emit(app, UpdateProgress { received, total });
        }
    }
    let _ = file.flush();
    drop(file);
    if total != 0 && received != total {
        let _ = std::fs::remove_file(&part_path);
        return Err(format!("下载不完整: {received}/{total} 字节"));
    }
    // 两个包体都不可能小于 1MB：空响应 / 截断页不能冒充包体
    if received < 1024 * 1024 {
        let _ = std::fs::remove_file(&part_path);
        return Err("下载的包体异常（过小），已丢弃".into());
    }
    std::fs::rename(&part_path, &final_path)
        .map_err(|error| format!("无法保存更新包: {error}"))?;
    *staged().lock().unwrap() = Some(StagedUpdate {
        path: final_path,
        version,
        kind,
    });
    let _ = UPDATE_PROGRESS.emit(app, UpdateProgress { received, total });
    Ok(())
}

// ---- 安装 ----

/// 便携版替换后的 exe 文件名：文件名即版本，便于用户分辨。用户改过名字
/// （或旧版未带版本号）时补上版本号，避免覆盖同名文件后无从判断版本。
fn versioned_exe_name(current: &Path, from: &str, to: &str) -> String {
    let stem = current
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("FloePod");
    let extension = current
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("exe");
    let renamed = if stem.contains(from) {
        stem.replacen(from, to, 1)
    } else {
        format!("{stem}-{to}")
    };
    format!("{renamed}.{extension}")
}

/// PowerShell 单引号字符串转义（' → ''），防止路径字符破坏脚本结构。
fn ps_quote(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

/// 便携版接力脚本：等进程退出 → 把暂存 exe 挪到带新版本号的名字上 →
/// 删旧程序 → 启动新版；挪动彻底失败时退回启动旧程序，保证应用总在运行。
fn replace_script(pid: u32, staged: &Path, target: &Path, current: &Path) -> String {
    format!(
        r#"$ErrorActionPreference = 'SilentlyContinue'
Wait-Process -Id {pid} -Timeout 120 -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 400
$src = '{}'; $dst = '{}'; $old = '{}'
for ($i = 0; $i -lt 120; $i++) {{
  try {{ Move-Item -LiteralPath $src -Destination $dst -Force -ErrorAction Stop; break }}
  catch {{ Start-Sleep -Milliseconds 500 }}
}}
if (Test-Path -LiteralPath $dst) {{
  if ($old -ne $dst) {{ Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue }}
  Start-Process -FilePath $dst
}} else {{
  Start-Process -FilePath $old
}}"#,
        ps_quote(staged),
        ps_quote(target),
        ps_quote(current),
    )
}

/// 安装版接力脚本：等进程退出 → 静默运行 NSIS 安装器（沿用上次安装路径
/// 原地升级并卸载旧版）→ 从原路径启动，安装器已把该路径的 exe 升级为新版。
fn silent_reinstall_script(pid: u32, setup: &Path, current: &Path) -> String {
    format!(
        r#"$ErrorActionPreference = 'SilentlyContinue'
Wait-Process -Id {pid} -Timeout 120 -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 400
Start-Process -FilePath '{}' -ArgumentList '/S' -Wait
Start-Process -FilePath '{}'"#,
        ps_quote(setup),
        ps_quote(current),
    )
}

/// 以 UTF-16LE + Base64 的 -EncodedCommand 投递脚本：不落盘、无引号歧义。
#[cfg(windows)]
fn spawn_relay_script(script: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let utf16: Vec<u8> = script
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-EncodedCommand", &encoded])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法启动安装脚本: {error}"))
}

#[cfg(not(windows))]
fn spawn_relay_script(_script: &str) -> Result<(), String> {
    Err("自动安装仅支持 Windows".into())
}

/// 从便携 zip 中解出主程序 exe，放在更新目录等待替换。
fn extract_portable_exe(staged: &StagedUpdate) -> Result<PathBuf, String> {
    let file =
        std::fs::File::open(&staged.path).map_err(|error| format!("无法读取更新包: {error}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|error| format!("更新包损坏: {error}"))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("更新包损坏: {error}"))?;
        let name = entry
            .name()
            .map_err(|error| format!("更新包条目名无效: {error}"))?
            .into_owned();
        // 打包脚本固定把主程序放在 zip 的 FloePod/ 一级目录下
        if name != "FloePod/FloePod.exe" {
            continue;
        }
        let out = staged
            .path
            .with_file_name(format!("FloePod-{}.exe", staged.version));
        let mut writer = std::fs::File::create(&out)
            .map_err(|error| format!("无法解出更新程序: {error}"))?;
        std::io::copy(&mut entry, &mut writer)
            .map_err(|error| format!("解出更新程序失败: {error}"))?;
        return Ok(out);
    }
    Err("更新包里没有找到 FloePod 主程序".into())
}

pub fn install(app: &AppHandle) -> Result<(), String> {
    if running_in_msix() {
        return Err("MSIX 侧载包由系统管理，请到 Release 页手动下载新版".into());
    }
    let staged_update = staged()
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| "还没有已下载的更新".to_string())?;
    let state = app.state::<crate::state::AppState>();
    // 正在复制 / 移动文件时强杀进程会留下半截文件：占不到锁就把暂存记录
    // 放回去，等用户稍后再试。
    let Ok(_guard) = state.file_ops.try_lock() else {
        *staged().lock().unwrap() = Some(staged_update);
        return Err("文件操作进行中，请稍后再试".into());
    };
    let current = std::env::current_exe().map_err(|error| format!("无法定位当前程序: {error}"))?;
    let script = match staged_update.kind {
        InstallKind::Portable => {
            let staged_exe = extract_portable_exe(&staged_update)?;
            let target = current.with_file_name(versioned_exe_name(
                &current,
                env!("CARGO_PKG_VERSION"),
                &staged_update.version,
            ));
            replace_script(
                std::process::id(),
                &staged_exe,
                &target,
                &current,
            )
        }
        InstallKind::Installer => {
            silent_reinstall_script(std::process::id(), &staged_update.path, &current)
        }
    };
    spawn_relay_script(&script)?;
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_exe_name_replaces_or_appends_the_version() {
        let current = Path::new("C:/tools/FloePod-2.0.0-beta1.exe");
        assert_eq!(
            versioned_exe_name(current, "2.0.0-beta1", "2.0.0-beta2"),
            "FloePod-2.0.0-beta2.exe"
        );
        // 用户改过名或旧版不带版本号：补上版本号而不是覆盖同名文件
        let plain = Path::new("C:/tools/FloePod.exe");
        assert_eq!(
            versioned_exe_name(plain, "2.0.0-beta1", "2.0.0"),
            "FloePod-2.0.0.exe"
        );
    }

    #[test]
    fn asset_names_follow_bundler_conventions() {
        assert_eq!(
            asset_name(InstallKind::Portable, "2.0.0-beta1"),
            "FloePod-2.0.0-beta1-win-x64-portable.zip"
        );
        assert_eq!(
            asset_name(InstallKind::Installer, "2.0.0-beta1"),
            "FloePod_2.0.0-beta1_x64-setup.exe"
        );
    }

    #[test]
    fn channel_gate_matches_the_release_workflow_rule() {
        assert!(accepts_tag("beta", "v2.0.0-beta1"));
        assert!(accepts_tag("beta", "v2.0.0"));
        assert!(!accepts_tag("stable", "v2.0.0-beta1"));
        assert!(accepts_tag("stable", "v2.0.0"));
        assert_eq!(parse_tag("v2.0.0-beta1").map(|v| v.to_string()).as_deref(), Some("2.0.0-beta1"));
        assert!(parse_tag("v2.0.0-beta1").is_some_and(|v| v < Version::parse("2.0.0").unwrap()));
    }

    #[test]
    fn html_notes_collapse_to_plain_text() {
        let html = "<h2>亮点</h2>\n<p>双通道 &amp; 自动安装</p>\n<ul><li>更省心</li><li>更快</li></ul>";
        assert_eq!(
            html_to_text(html, 100),
            "亮点\n双通道 & 自动安装\n更省心\n更快"
        );
        let long = "字".repeat(4000);
        let truncated = html_to_text(&long, 3000);
        assert_eq!(truncated.chars().count(), 3001);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn atom_entries_extract_tag_url_and_notes() {
        let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>tag:github.com,2008:Repository/1/v2.0.0-beta1</id>
    <link rel="alternate" type="text/html" href="https://github.com/MogroWang/FloePod/releases/tag/v2.0.0-beta1"/>
    <title>FloePod 2.0.0-beta1</title>
    <content type="html">&lt;p&gt;双通道 &amp;amp; 自动安装&lt;/p&gt;</content>
  </entry>
  <entry>
    <id>tag:github.com,2008:Repository/1/v2.0.0</id>
    <link rel="alternate" type="text/html" href="https://github.com/MogroWang/FloePod/releases/tag/v2.0.0"/>
    <content type="html">&lt;p&gt;正式版&lt;/p&gt;</content>
  </entry>
</feed>"#;
        let entries = parse_atom(body).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].tag, "v2.0.0-beta1");
        assert_eq!(entries[0].notes_html, "<p>双通道 &amp; 自动安装</p>");
        assert_eq!(entries[1].tag, "v2.0.0");
    }
}
