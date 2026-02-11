mod downloader;

use downloader::{
    build_output_filename, cleanup_temp, download_segments, find_ffmpeg, get_clip_info,
    get_video_info, get_video_info_with_cookies, merge_segments,
    parse_segments, remux_with_ffmpeg, DownloadProgress,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::{Emitter, Manager};

#[cfg(target_os = "windows")]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Serialize)]
struct VideoQuality {
    id: String,
    width: u32,
    height: u32,
    bandwidth: u64,
    label: String,
}

#[derive(Serialize)]
struct VodInfo {
    title: String,
    channel: String,
    duration: u64,
    thumbnail: String,
    qualities: Vec<VideoQuality>,
}

#[derive(Serialize)]
struct ClipInfoResp {
    title: String,
    channel: String,
    thumbnail: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct Credentials {
    nid_aut: String,
    nid_ses: String,
}

fn get_credentials_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let app_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("앱 데이터 디렉토리를 찾을 수 없습니다: {}", e))?;

    fs::create_dir_all(&app_dir)
        .map_err(|e| format!("디렉토리 생성 실패: {}", e))?;

    Ok(app_dir.join("credentials.json"))
}

#[tauri::command]
async fn save_credentials(
    app: tauri::AppHandle,
    nid_aut: String,
    nid_ses: String,
) -> Result<(), String> {
    let creds = Credentials { nid_aut, nid_ses };
    let path = get_credentials_path(&app)?;

    let json = serde_json::to_string_pretty(&creds)
        .map_err(|e| format!("JSON 직렬화 실패: {}", e))?;

    fs::write(&path, json)
        .map_err(|e| format!("파일 쓰기 실패: {}", e))?;

    Ok(())
}

#[tauri::command]
async fn load_credentials(app: tauri::AppHandle) -> Result<Option<Credentials>, String> {
    let path = get_credentials_path(&app)?;

    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)
        .map_err(|e| format!("파일 읽기 실패: {}", e))?;

    let creds: Credentials = serde_json::from_str(&content)
        .map_err(|e| format!("JSON 파싱 실패: {}", e))?;

    Ok(Some(creds))
}

#[tauri::command]
async fn open_login_webview(app: tauri::AppHandle) -> Result<String, String> {
    use tauri::{WebviewWindowBuilder, WebviewUrl};

    // 이미 열려있으면 포커스
    if let Some(existing) = app.get_webview_window("naver-login") {
        let _ = existing.set_focus();
        return Ok("login_webview_focused".to_string());
    }

    let login_url = "https://nid.naver.com/nidlogin.login?mode=form&url=https%3A%2F%2Fchzzk.naver.com%2F";

    let mut builder = WebviewWindowBuilder::new(
        &app,
        "naver-login",
        WebviewUrl::External(login_url.parse().unwrap()),
    )
    .title("네이버 로그인")
    .inner_size(500.0, 700.0)
    .resizable(true)
    .center();

    // Windows: on_navigation으로 로그인 완료 감지 후 쿠키 자동 추출
    #[cfg(target_os = "windows")]
    {
        let app_handle = app.clone();
        let extracted = Arc::new(AtomicBool::new(false));

        builder = builder.on_navigation(move |url| {
            eprintln!("🌐 Navigation: {}", url.as_str());

            // chzzk.naver.com으로 리다이렉트되면 로그인 완료
            if url.host_str() == Some("chzzk.naver.com")
                && !extracted.swap(true, Ordering::SeqCst)
            {
                eprintln!("✅ Login redirect detected - extracting cookies...");
                let app = app_handle.clone();

                std::thread::spawn(move || {
                    // 쿠키가 완전히 저장될 때까지 잠시 대기
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    // AppHandle에서 웹뷰 참조 획득
                    if let Some(wv) = app.get_webview_window("naver-login") {
                        extract_and_save_cookies(wv, app.clone());
                    }
                });
            }
            true
        });
    }

    let _webview = builder
        .build()
        .map_err(|e| format!("로그인 창 생성 실패: {}", e))?;

    eprintln!("🌐 Login webview opened");
    Ok("login_webview_opened".to_string())
}

/// WebView2 CookieManager를 통해 NID_AUT/NID_SES 쿠키를 추출하고 저장
#[cfg(target_os = "windows")]
fn extract_and_save_cookies(webview: tauri::WebviewWindow, app: tauri::AppHandle) {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::sync_channel::<Vec<(String, String)>>(1);

    let result = webview.with_webview(move |platform_webview| {
        unsafe {
            use webview2_com::Microsoft::Web::WebView2::Win32::*;
            use windows_core::Interface;

            let controller = platform_webview.controller();
            let core: ICoreWebView2 = controller.CoreWebView2().unwrap();
            let core2: ICoreWebView2_2 = core.cast().unwrap();
            let cookie_manager = core2.CookieManager().unwrap();

            let handler: ICoreWebView2GetCookiesCompletedHandler =
                CookieCompletedHandler { sender: tx }.into();

            cookie_manager
                .GetCookies(
                    windows_core::w!("https://chzzk.naver.com"),
                    &handler,
                )
                .unwrap();
        }
    });

    if let Err(e) = result {
        eprintln!("❌ with_webview failed: {:?}", e);
        return;
    }

    match rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(cookies) => {
            let mut nid_aut = String::new();
            let mut nid_ses = String::new();

            for (name, value) in &cookies {
                let preview = if value.len() > 20 {
                    format!("{}...", &value[..20])
                } else {
                    value.clone()
                };
                eprintln!("🍪 Cookie: {}={}", name, preview);

                if name == "NID_AUT" {
                    nid_aut = value.clone();
                }
                if name == "NID_SES" {
                    nid_ses = value.clone();
                }
            }

            if !nid_aut.is_empty() && !nid_ses.is_empty() {
                eprintln!("✅ Successfully extracted NID_AUT and NID_SES");
                tauri::async_runtime::spawn(async move {
                    let _ =
                        save_credentials(app.clone(), nid_aut.clone(), nid_ses.clone()).await;
                    let _ = app.emit("login-success", Credentials { nid_aut, nid_ses });
                    // 로그인 웹뷰 자동 닫기
                    if let Some(wv) = app.get_webview_window("naver-login") {
                        let _ = wv.close();
                    }
                });
            } else {
                eprintln!(
                    "⚠️ NID_AUT or NID_SES not found ({} cookies total)",
                    cookies.len()
                );
            }
        }
        Err(e) => {
            eprintln!("❌ Cookie extraction timeout: {}", e);
        }
    }
}

/// WebView2 GetCookies COM 콜백 핸들러
#[cfg(target_os = "windows")]
#[windows_core::implement(
    webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2GetCookiesCompletedHandler
)]
struct CookieCompletedHandler {
    sender: std::sync::mpsc::SyncSender<Vec<(String, String)>>,
}

#[cfg(target_os = "windows")]
impl
    webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2GetCookiesCompletedHandler_Impl
    for CookieCompletedHandler_Impl
{
    fn Invoke(
        &self,
        errorcode: windows_core::HRESULT,
        result: windows_core::Ref<
            '_,
            webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CookieList,
        >,
    ) -> windows_core::Result<()> {
        let mut cookies = Vec::new();
        if errorcode.is_ok() {
            if let Ok(list) = result.ok() {
                unsafe {
                    let mut count: u32 = 0;
                    list.Count(&mut count)?;
                    for i in 0..count {
                        let cookie = list.GetValueAtIndex(i)?;
                        let mut name_ptr = windows_core::PWSTR::null();
                        cookie.Name(&mut name_ptr)?;
                        let name = name_ptr.to_string().unwrap_or_default();

                        let mut value_ptr = windows_core::PWSTR::null();
                        cookie.Value(&mut value_ptr)?;
                        let value = value_ptr.to_string().unwrap_or_default();

                        cookies.push((name, value));
                    }
                }
            }
        }
        let _ = self.sender.send(cookies);
        Ok(())
    }
}

#[tauri::command]
async fn close_login_webview(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(webview) = app.get_webview_window("naver-login") {
        webview
            .close()
            .map_err(|e| format!("창 닫기 실패: {}", e))?;
        eprintln!("🔒 Login webview closed");
    }
    Ok(())
}

#[tauri::command]
async fn check_ffmpeg(app: tauri::AppHandle) -> Result<bool, String> {
    Ok(find_ffmpeg(&app).await.is_some())
}

#[tauri::command]
async fn install_ffmpeg(app: tauri::AppHandle) -> Result<String, String> {
    let path = downloader::download_ffmpeg(&app).await?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
async fn download_clip_cmd(
    app: tauri::AppHandle,
    clip_uid: String,
    output_dir: String,
) -> Result<String, String> {
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            stage: "info".into(),
            current: 0,
            total: 1,
            message: "클립 정보를 가져오는 중...".into(),
        },
    );

    let clip_info = get_clip_info(&clip_uid).await?;

    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            stage: "info".into(),
            current: 1,
            total: 1,
            message: format!("{} - {}", clip_info.channel, clip_info.title),
        },
    );

    let output_path = downloader::download_clip(&app, &clip_info, &output_dir).await?;

    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            stage: "complete".into(),
            current: 1,
            total: 1,
            message: "다운로드 완료!".into(),
        },
    );

    Ok(output_path)
}

#[tauri::command]
async fn fetch_clip_info(clip_uid: String) -> Result<ClipInfoResp, String> {
    let info = get_clip_info(&clip_uid).await?;
    Ok(ClipInfoResp {
        title: info.title,
        channel: info.channel,
        thumbnail: info.thumbnail,
    })
}

#[tauri::command]
async fn fetch_video_info(app: tauri::AppHandle, video_id: String) -> Result<VodInfo, String> {
    // 저장된 쿠키 불러오기
    let creds = load_credentials(app).await?;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("Referer", "https://chzzk.naver.com/".parse().unwrap());

    // 쿠키가 있으면 추가
    if let Some(c) = creds {
        let cookie_value = format!("NID_AUT={}; NID_SES={}", c.nid_aut, c.nid_ses);
        if let Ok(cookie) = cookie_value.parse() {
            headers.insert("Cookie", cookie);
            eprintln!("🔐 Using saved credentials for video info request");
        }
    }

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .default_headers(headers)
        .build()
        .map_err(|e| format!("HTTP 클라이언트 생성 실패: {}", e))?;

    let api_url = format!("https://api.chzzk.naver.com/service/v3/videos/{}", video_id);
    let resp: serde_json::Value = client
        .get(&api_url)
        .send()
        .await
        .map_err(|e| format!("API 요청 실패: {}", e))?
        .json()
        .await
        .map_err(|e| format!("JSON 파싱 실패: {}", e))?;

    let content = resp.get("content").ok_or("API 응답에 content가 없습니다")?;

    // 디버깅: VOD API 응답 출력
    eprintln!("📹 VOD API response content: {}", serde_json::to_string_pretty(content).unwrap_or_default());

    let title = content
        .get("videoTitle")
        .and_then(|v| v.as_str())
        .unwrap_or("video")
        .to_string();
    let channel = content
        .get("channel")
        .and_then(|c| c.get("channelName"))
        .and_then(|v| v.as_str())
        .unwrap_or("channel")
        .to_string();
    let duration = content.get("duration").and_then(|v| v.as_u64()).unwrap_or(0);
    let thumbnail = content
        .get("thumbnailImageUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // 화질 정보 가져오기
    let mut qualities = Vec::new();

    // HLS 또는 DASH 화질 목록 가져오기
    if let Some(media_json_str) = content.get("liveRewindPlaybackJson").and_then(|v| v.as_str()) {
        // HLS 방식 - master playlist에서 화질 목록 추출
        if let Ok(media_data) = serde_json::from_str::<serde_json::Value>(media_json_str) {
            if let Some(master_url) = media_data
                .get("media")
                .and_then(|m| m.as_array())
                .and_then(|arr| arr.first())
                .and_then(|item| item.get("path"))
                .and_then(|v| v.as_str())
            {
                // Master playlist 가져오기
                if let Ok(master_resp) = client.get(master_url).send().await {
                    if let Ok(master_text) = master_resp.text().await {
                        // #EXT-X-STREAM-INF 라인 파싱
                        let lines: Vec<&str> = master_text.lines().collect();
                        let mut i = 0;
                        while i < lines.len() {
                            let line = lines[i];
                            if line.starts_with("#EXT-X-STREAM-INF:") {
                                // bandwidth와 resolution 추출
                                let mut bandwidth = 0u64;
                                let mut width = 0u32;
                                let mut height = 0u32;

                                // #EXT-X-STREAM-INF: 제거하고 파라미터만 추출
                                let params_str = line.strip_prefix("#EXT-X-STREAM-INF:").unwrap_or("");

                                // BANDWIDTH 추출 (정규식 대신 문자열 검색)
                                if let Some(bw_start) = params_str.find("BANDWIDTH=") {
                                    let bw_str = &params_str[bw_start + 10..];
                                    if let Some(comma_pos) = bw_str.find(',') {
                                        bandwidth = bw_str[..comma_pos].parse().unwrap_or(0);
                                    } else {
                                        bandwidth = bw_str.parse().unwrap_or(0);
                                    }
                                }

                                // RESOLUTION 추출
                                if let Some(res_start) = params_str.find("RESOLUTION=") {
                                    let res_str = &params_str[res_start + 11..];
                                    let res_end = res_str.find(',').unwrap_or(res_str.len());
                                    let resolution = &res_str[..res_end];
                                    let parts: Vec<&str> = resolution.split('x').collect();
                                    if parts.len() == 2 {
                                        width = parts[0].parse().unwrap_or(0);
                                        height = parts[1].parse().unwrap_or(0);
                                    }
                                }

                                // 다음 줄이 variant playlist URL
                                if i + 1 < lines.len() {
                                    let variant_url = lines[i + 1].trim();
                                    if !variant_url.starts_with('#') && !variant_url.is_empty() {
                                        let label = if height > 0 {
                                            format!("{}p ({:.1}Mbps)", height, bandwidth as f64 / 1_000_000.0)
                                        } else {
                                            format!("{:.1}Mbps", bandwidth as f64 / 1_000_000.0)
                                        };

                                        qualities.push(VideoQuality {
                                            id: variant_url.to_string(),
                                            width,
                                            height,
                                            bandwidth,
                                            label,
                                        });
                                    }
                                }
                            }
                            i += 1;
                        }
                    }
                }
            }
        }

        // HLS 화질 목록 정렬 (bandwidth 기준 내림차순)
        qualities.sort_by(|a, b| b.bandwidth.cmp(&a.bandwidth));
    } else if content.get("liveRewindPlaybackJson").and_then(|v| v.as_str()).is_none() {
        // DASH 방식 - 기존 로직
        if let (Some(video_id_key), Some(in_key)) = (
            content.get("videoId").and_then(|v| v.as_str()),
            content.get("inKey").and_then(|v| v.as_str()),
        ) {
            let playback_url = format!(
                "https://apis.naver.com/neonplayer/vodplay/v2/playback/{}?key={}",
                video_id_key, in_key
            );

            if let Ok(playback_resp) = client.get(&playback_url).send().await {
                if let Ok(playback_json) = playback_resp.json::<serde_json::Value>().await {
                    if let Some(first_period) = playback_json
                        .get("period")
                        .and_then(|p| p.as_array())
                        .and_then(|arr| arr.first())
                    {
                        if let Some(hls_set) = first_period
                            .get("adaptationSet")
                            .and_then(|a| a.as_array())
                            .and_then(|sets| {
                                sets.iter().find(|s| {
                                    s.get("mimeType").and_then(|m| m.as_str())
                                        == Some("video/mp2t")
                                })
                            })
                        {
                            if let Some(representations) =
                                hls_set.get("representation").and_then(|r| r.as_array())
                            {
                                for rep in representations {
                                    let id = rep.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                    let width =
                                        rep.get("width").and_then(|v| v.as_u64()).unwrap_or(0)
                                            as u32;
                                    let height =
                                        rep.get("height").and_then(|v| v.as_u64()).unwrap_or(0)
                                            as u32;
                                    let bandwidth =
                                        rep.get("bandwidth").and_then(|v| v.as_u64()).unwrap_or(0);

                                    let label = if width > 0 && height > 0 {
                                        format!("{}p ({:.1}Mbps)", height, bandwidth as f64 / 1_000_000.0)
                                    } else {
                                        format!("{:.1}Mbps", bandwidth as f64 / 1_000_000.0)
                                    };

                                    qualities.push(VideoQuality {
                                        id: id.to_string(),
                                        width,
                                        height,
                                        bandwidth,
                                        label,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // bandwidth 기준 내림차순 정렬 (최고 화질이 먼저)
    qualities.sort_by(|a, b| b.bandwidth.cmp(&a.bandwidth));

    Ok(VodInfo {
        title,
        channel,
        duration,
        thumbnail,
        qualities,
    })
}

#[tauri::command]
async fn download_vod(
    app: tauri::AppHandle,
    video_id: String,
    start_time: String,
    end_time: String,
    output_dir: String,
    quality_id: Option<String>,
) -> Result<String, String> {
    // 0. ffmpeg 확인
    let ffmpeg_path = find_ffmpeg(&app)
        .await
        .ok_or("ffmpeg를 찾을 수 없습니다. 먼저 ffmpeg를 설치해주세요.")?;

    // 1. 비디오 정보 가져오기
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            stage: "info".into(),
            current: 0,
            total: 1,
            message: "비디오 정보를 가져오는 중...".into(),
        },
    );

    // 저장된 쿠키 불러오기
    let creds = load_credentials(app.clone()).await?;
    let info = if let Some(c) = creds {
        get_video_info_with_cookies(&video_id, Some(c.nid_aut), Some(c.nid_ses)).await?
    } else {
        get_video_info(&video_id).await?
    };
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            stage: "info".into(),
            current: 1,
            total: 1,
            message: format!("{} - {}", info.channel, info.title),
        },
    );

    // 2. 세그먼트 URL 파싱 (DASH 또는 HLS)
    let quality_ref = quality_id.as_deref();
    let segments = if info.is_dash {
        let dash_video_id = info.dash_video_id.as_ref().ok_or("DASH videoId가 없습니다")?;
        let dash_in_key = info.dash_in_key.as_ref().ok_or("DASH inKey가 없습니다")?;
        downloader::parse_dash_segments(dash_video_id, dash_in_key, &start_time, &end_time, quality_ref).await?
    } else {
        parse_segments(&info.master_url, &start_time, &end_time, quality_ref).await?
    };

    if segments.is_empty() {
        return Err("다운로드할 세그먼트가 없습니다".into());
    }

    // 3. 세그먼트 다운로드
    let temp_dir = PathBuf::from(&output_dir).join(format!("temp_{}", video_id));
    download_segments(&app, &segments, &temp_dir).await?;

    // 4. 세그먼트 병합
    let combined_path = merge_segments(&app, segments.len(), &temp_dir).await?;

    // 5. ffmpeg로 리먹싱
    let output_path = build_output_filename(&info, &start_time, &end_time, &output_dir);
    remux_with_ffmpeg(&app, &ffmpeg_path, &combined_path, &output_path).await?;

    // 6. 임시 파일 정리
    let _ = cleanup_temp(&temp_dir).await;

    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            stage: "complete".into(),
            current: 1,
            total: 1,
            message: "다운로드 완료!".into(),
        },
    );

    Ok(output_path.to_string_lossy().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            download_vod,
            download_clip_cmd,
            check_ffmpeg,
            install_ffmpeg,
            fetch_video_info,
            fetch_clip_info,
            save_credentials,
            load_credentials,
            open_login_webview,
            close_login_webview
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
