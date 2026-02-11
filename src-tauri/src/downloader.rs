use futures::stream::{self, StreamExt};
use regex::Regex;
use reqwest::Client;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};
use tokio::fs;
use tokio::io::AsyncWriteExt;

const FFMPEG_DOWNLOAD_URL: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip";

#[derive(Clone, Serialize)]
pub struct DownloadProgress {
    pub stage: String,
    pub current: u32,
    pub total: u32,
    pub message: String,
}

#[derive(Debug)]
pub struct VideoInfo {
    pub title: String,
    pub channel: String,
    pub master_url: String,
    pub duration: u64,
    pub thumbnail: String,
    // DASH 정보
    pub is_dash: bool,
    pub dash_video_id: Option<String>,
    pub dash_in_key: Option<String>,
}

#[derive(Debug)]
pub struct ClipInfo {
    pub title: String,
    pub channel: String,
    pub mp4_url: String,
    pub thumbnail: String,
}

fn build_client() -> Client {
    build_client_with_cookies(None, None)
}

fn build_client_with_cookies(nid_aut: Option<String>, nid_ses: Option<String>) -> Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("Referer", "https://chzzk.naver.com/".parse().unwrap());

    // 쿠키가 있으면 추가
    if let (Some(aut), Some(ses)) = (nid_aut, nid_ses) {
        let cookie_value = format!("NID_AUT={}; NID_SES={}", aut, ses);
        if let Ok(cookie) = cookie_value.parse() {
            headers.insert("Cookie", cookie);
        }
    }

    Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .default_headers(headers)
        .build()
        .expect("Failed to build HTTP client")
}

fn time_to_sec(t: &str) -> f64 {
    if t.is_empty() {
        return 0.0;
    }
    let parts: Vec<f64> = t.split(':').filter_map(|p| p.parse().ok()).collect();
    match parts.len() {
        3 => parts[0] * 3600.0 + parts[1] * 60.0 + parts[2],
        2 => parts[0] * 60.0 + parts[1],
        1 => parts[0],
        _ => 0.0,
    }
}

fn resolve_url(base: &str, relative: &str) -> String {
    if relative.starts_with("http://") || relative.starts_with("https://") {
        return relative.to_string();
    }
    // Strip query string before finding last '/' in path
    let base_path = match base.find('?') {
        Some(q) => &base[..q],
        None => base,
    };
    if let Some(pos) = base_path.rfind('/') {
        format!("{}/{}", &base[..pos], relative)
    } else {
        relative.to_string()
    }
}

fn sanitize_filename(s: &str) -> String {
    let re = Regex::new(r#"[\\/*?:"<>|]"#).unwrap();
    re.replace_all(s, "").to_string()
}

// ── ffmpeg 관련 ─────────────────────────────────────────

fn app_ffmpeg_path(app: &AppHandle) -> Result<PathBuf, String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("앱 데이터 경로를 가져올 수 없습니다: {}", e))?;
    Ok(data_dir.join("ffmpeg.exe"))
}

pub async fn find_ffmpeg(app: &AppHandle) -> Option<PathBuf> {
    // 1. 시스템 PATH 체크 (spawn_blocking으로 안정적으로)
    let found_in_path = tokio::task::spawn_blocking(|| {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);

    if found_in_path {
        return Some(PathBuf::from("ffmpeg"));
    }

    // 2. 앱 로컬 데이터 폴더 체크
    if let Ok(path) = app_ffmpeg_path(app) {
        if path.exists() {
            return Some(path);
        }
    }

    None
}

pub async fn download_ffmpeg(app: &AppHandle) -> Result<PathBuf, String> {
    let ffmpeg_dest = app_ffmpeg_path(app)?;

    if ffmpeg_dest.exists() {
        return Ok(ffmpeg_dest);
    }

    if let Some(parent) = ffmpeg_dest.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("폴더 생성 실패: {}", e))?;
    }

    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            stage: "ffmpeg-install".into(),
            current: 0,
            total: 100,
            message: "ffmpeg 다운로드 중...".into(),
        },
    );

    let client = Client::builder()
        .user_agent("chzzk-downloader")
        .build()
        .map_err(|e| format!("HTTP 클라이언트 생성 실패: {}", e))?;

    let resp = client
        .get(FFMPEG_DOWNLOAD_URL)
        .send()
        .await
        .map_err(|e| format!("ffmpeg 다운로드 요청 실패: {}", e))?;

    let total_size = resp.content_length().unwrap_or(0);
    let temp_zip = ffmpeg_dest.with_file_name("ffmpeg_temp.zip");

    let mut file = fs::File::create(&temp_zip)
        .await
        .map_err(|e| format!("임시 파일 생성 실패: {}", e))?;

    let mut downloaded: u64 = 0;
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("다운로드 중 오류: {}", e))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("파일 쓰기 실패: {}", e))?;

        downloaded += chunk.len() as u64;
        let percent = if total_size > 0 {
            (downloaded * 100 / total_size) as u32
        } else {
            0
        };
        let mb_done = downloaded / (1024 * 1024);
        let mb_total = total_size / (1024 * 1024);

        let _ = app.emit(
            "download-progress",
            DownloadProgress {
                stage: "ffmpeg-install".into(),
                current: percent,
                total: 100,
                message: format!("ffmpeg 다운로드 중... ({}MB / {}MB)", mb_done, mb_total),
            },
        );
    }

    drop(file);

    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            stage: "ffmpeg-install".into(),
            current: 100,
            total: 100,
            message: "ffmpeg 압축 해제 중...".into(),
        },
    );

    let zip_path = temp_zip.clone();
    let dest_path = ffmpeg_dest.clone();

    tokio::task::spawn_blocking(move || {
        let file =
            std::fs::File::open(&zip_path).map_err(|e| format!("ZIP 파일 열기 실패: {}", e))?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| format!("ZIP 파싱 실패: {}", e))?;

        let mut found = false;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("ZIP 엔트리 읽기 실패: {}", e))?;

            let name = entry.name().to_string();
            if name.ends_with("bin/ffmpeg.exe") {
                let mut out = std::fs::File::create(&dest_path)
                    .map_err(|e| format!("ffmpeg.exe 생성 실패: {}", e))?;
                std::io::copy(&mut entry, &mut out)
                    .map_err(|e| format!("ffmpeg.exe 추출 실패: {}", e))?;
                found = true;
                break;
            }
        }

        if !found {
            return Err("ZIP에서 ffmpeg.exe를 찾을 수 없습니다".into());
        }

        let _ = std::fs::remove_file(&zip_path);
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("추출 작업 실패: {}", e))??;

    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            stage: "ffmpeg-install".into(),
            current: 100,
            total: 100,
            message: "ffmpeg 설치 완료!".into(),
        },
    );

    Ok(ffmpeg_dest)
}

// ── 클립 관련 ─────────────────────────────────────────

pub async fn get_clip_info(clip_uid: &str) -> Result<ClipInfo, String> {
    let client = build_client();

    // 1단계: play-info에서 videoId, inKey, 제목, 채널 가져오기
    let api_url = format!(
        "https://api.chzzk.naver.com/service/v1/play-info/clip/{}",
        clip_uid
    );

    let resp: serde_json::Value = client
        .get(&api_url)
        .send()
        .await
        .map_err(|e| format!("클립 API 요청 실패: {}", e))?
        .json()
        .await
        .map_err(|e| format!("클립 JSON 파싱 실패: {}", e))?;

    let content = resp
        .get("content")
        .ok_or("클립 API 응답에 content가 없습니다")?;

    // 디버깅: 사용 가능한 모든 필드 출력
    eprintln!("📋 Clip API response content: {}", serde_json::to_string_pretty(content).unwrap_or_default());

    let title = content
        .get("contentTitle")
        .and_then(|v| v.as_str())
        .unwrap_or("clip")
        .to_string();

    let channel = content
        .get("ownerChannel")
        .and_then(|c| c.get("channelName"))
        .and_then(|v| v.as_str())
        .unwrap_or("channel")
        .to_string();

    let video_id = content
        .get("videoId")
        .and_then(|v| v.as_str())
        .ok_or("클립 videoId를 찾을 수 없습니다")?;

    let in_key = content
        .get("inKey")
        .and_then(|v| v.as_str())
        .ok_or("클립 inKey를 찾을 수 없습니다")?;

    // 2단계: vodplay API에서 직접 MP4 URL 가져오기
    let playback_url = format!(
        "https://apis.naver.com/neonplayer/vodplay/v2/playback/{}?key={}",
        video_id, in_key
    );

    let playback_resp: serde_json::Value = client
        .get(&playback_url)
        .send()
        .await
        .map_err(|e| format!("재생 정보 요청 실패: {}", e))?
        .json()
        .await
        .map_err(|e| format!("재생 정보 JSON 파싱 실패: {}", e))?;

    let first_period = playback_resp
        .get("period")
        .and_then(|p| p.as_array())
        .and_then(|arr| arr.first());

    // period[0].adaptationSet에서 mimeType이 "video/mp4"인 항목 찾기
    let mp4_url = first_period
        .and_then(|period| period.get("adaptationSet"))
        .and_then(|a| a.as_array())
        .and_then(|sets| {
            sets.iter().find(|s| {
                s.get("mimeType")
                    .and_then(|m| m.as_str())
                    == Some("video/mp4")
            })
        })
        .and_then(|set| set.get("representation"))
        .and_then(|r| r.as_array())
        .and_then(|reps| reps.first())
        .and_then(|rep| rep.get("baseURL"))
        .and_then(|b| b.as_array())
        .and_then(|urls| urls.first())
        .and_then(|url| url.get("value"))
        .and_then(|v| v.as_str())
        .ok_or("클립 MP4 URL을 찾을 수 없습니다")?
        .to_string();

    // supplementalProperty → thumbnailSet → 첫 번째 썸네일 URL
    let thumbnail = first_period
        .and_then(|p| p.get("supplementalProperty"))
        .and_then(|sp| sp.as_array())
        .and_then(|arr| arr.first())
        .and_then(|sp| sp.get("any"))
        .and_then(|any| any.as_array())
        .and_then(|arr| arr.iter().find(|item| item.get("thumbnailSet").is_some()))
        .and_then(|item| item.get("thumbnailSet"))
        .and_then(|ts| ts.as_array())
        .and_then(|arr| arr.first())
        .and_then(|tset| tset.get("thumbnail"))
        .and_then(|t| t.as_array())
        .and_then(|arr| arr.first())
        .and_then(|thumb| thumb.get("source"))
        .and_then(|s| s.get("value"))
        .and_then(|v| v.as_str())
        .map(|url| {
            // ?type=s80 → 제거하거나 큰 사이즈로 변경
            if let Some(pos) = url.find("?type=") {
                url[..pos].to_string()
            } else {
                url.to_string()
            }
        })
        .unwrap_or_default();

    Ok(ClipInfo {
        title,
        channel,
        mp4_url,
        thumbnail,
    })
}

pub async fn download_clip(
    app: &AppHandle,
    clip_info: &ClipInfo,
    output_dir: &str,
) -> Result<String, String> {
    let safe_channel = sanitize_filename(&clip_info.channel);
    let safe_title = sanitize_filename(&clip_info.title);
    let filename = format!("{}_{}.mp4", safe_channel, safe_title);
    let output_path = Path::new(output_dir).join(&filename);

    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            stage: "downloading".into(),
            current: 0,
            total: 100,
            message: "클립 다운로드 중...".into(),
        },
    );

    let client = build_client();
    let resp = client
        .get(&clip_info.mp4_url)
        .send()
        .await
        .map_err(|e| format!("클립 다운로드 실패: {}", e))?;

    let total_size = resp.content_length().unwrap_or(0);
    let mut file = fs::File::create(&output_path)
        .await
        .map_err(|e| format!("파일 생성 실패: {}", e))?;

    let mut downloaded: u64 = 0;
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("다운로드 중 오류: {}", e))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("파일 쓰기 실패: {}", e))?;

        downloaded += chunk.len() as u64;
        let percent = if total_size > 0 {
            (downloaded * 100 / total_size) as u32
        } else {
            0
        };

        let _ = app.emit(
            "download-progress",
            DownloadProgress {
                stage: "downloading".into(),
                current: percent,
                total: 100,
                message: format!(
                    "클립 다운로드 중... ({}MB / {}MB)",
                    downloaded / (1024 * 1024),
                    total_size / (1024 * 1024)
                ),
            },
        );
    }

    Ok(output_path.to_string_lossy().to_string())
}

// ── VOD 관련 ─────────────────────────────────────────

pub async fn get_video_info(video_id: &str) -> Result<VideoInfo, String> {
    get_video_info_with_cookies(video_id, None, None).await
}

pub async fn get_video_info_with_cookies(
    video_id: &str,
    nid_aut: Option<String>,
    nid_ses: Option<String>,
) -> Result<VideoInfo, String> {
    let client = build_client_with_cookies(nid_aut, nid_ses);
    let api_url = format!(
        "https://api.chzzk.naver.com/service/v3/videos/{}",
        video_id
    );

    let resp: serde_json::Value = client
        .get(&api_url)
        .send()
        .await
        .map_err(|e| format!("API 요청 실패: {}", e))?
        .json()
        .await
        .map_err(|e| format!("JSON 파싱 실패: {}", e))?;

    let content = resp
        .get("content")
        .ok_or("API 응답에 content가 없습니다")?;

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

    let duration = content
        .get("duration")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let thumbnail = content
        .get("thumbnailImageUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // liveRewindPlaybackJson이 있으면 HLS, 없으면 DASH
    let (master_url, is_dash, dash_video_id, dash_in_key) = if let Some(media_json_str) = content
        .get("liveRewindPlaybackJson")
        .and_then(|v| v.as_str())
    {
        // 기존 HLS 방식
        let media_data: serde_json::Value = serde_json::from_str(media_json_str)
            .map_err(|e| format!("미디어 JSON 파싱 실패: {}", e))?;

        let url = media_data
            .get("media")
            .and_then(|m| m.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("path"))
            .and_then(|v| v.as_str())
            .ok_or("Master playlist URL을 찾을 수 없습니다")?
            .to_string();

        (url, false, None, None)
    } else {
        // DASH 방식 - videoId와 inKey 저장
        let video_id_key = content
            .get("videoId")
            .and_then(|v| v.as_str())
            .ok_or("videoId를 찾을 수 없습니다")?
            .to_string();

        let in_key = content
            .get("inKey")
            .and_then(|v| v.as_str())
            .ok_or("inKey를 찾을 수 없습니다")?
            .to_string();

        // placeholder URL (실제로는 사용 안 함)
        ("DASH".to_string(), true, Some(video_id_key), Some(in_key))
    };

    Ok(VideoInfo {
        title,
        channel,
        master_url,
        duration,
        thumbnail,
        is_dash,
        dash_video_id,
        dash_in_key,
    })
}

pub async fn parse_dash_segments(
    video_id: &str,
    in_key: &str,
    start_time: &str,
    end_time: &str,
    quality_id: Option<&str>,
) -> Result<Vec<String>, String> {
    let client = build_client();

    // DASH playback API 호출
    let playback_url = format!(
        "https://apis.naver.com/neonplayer/vodplay/v2/playback/{}?key={}",
        video_id, in_key
    );

    let playback_resp: serde_json::Value = client
        .get(&playback_url)
        .send()
        .await
        .map_err(|e| format!("재생 정보 요청 실패: {}", e))?
        .json()
        .await
        .map_err(|e| format!("재생 정보 JSON 파싱 실패: {}", e))?;

    // HLS adaptationSet 찾기 (video/mp2t)
    let first_period = playback_resp
        .get("period")
        .and_then(|p| p.as_array())
        .and_then(|arr| arr.first())
        .ok_or("period를 찾을 수 없습니다")?;

    let hls_set = first_period
        .get("adaptationSet")
        .and_then(|a| a.as_array())
        .and_then(|sets| {
            sets.iter().find(|s| {
                s.get("mimeType").and_then(|m| m.as_str()) == Some("video/mp2t")
            })
        })
        .ok_or("HLS adaptationSet을 찾을 수 없습니다")?;

    // representation 선택 (화질 ID 지정 또는 최고 화질)
    let representations = hls_set
        .get("representation")
        .and_then(|r| r.as_array())
        .ok_or("representation을 찾을 수 없습니다")?;

    let selected_rep = if let Some(qid) = quality_id {
        // 지정된 화질 ID로 찾기
        representations
            .iter()
            .find(|r| r.get("id").and_then(|v| v.as_str()) == Some(qid))
            .ok_or(format!("화질 ID '{}'를 찾을 수 없습니다", qid))?
    } else {
        // 최고 bandwidth의 representation 선택
        representations
            .iter()
            .max_by_key(|r| r.get("bandwidth").and_then(|b| b.as_u64()).unwrap_or(0))
            .ok_or("최고 품질 representation을 찾을 수 없습니다")?
    };

    let best_rep = selected_rep;

    let rep_id = best_rep
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("representation ID를 찾을 수 없습니다")?;

    let base_url = best_rep
        .get("baseURL")
        .and_then(|b| b.as_array())
        .and_then(|arr| arr.first())
        .and_then(|u| u.get("value"))
        .and_then(|v| v.as_str())
        .ok_or("baseURL을 찾을 수 없습니다")?;

    let seg_template = best_rep
        .get("segmentTemplate")
        .ok_or("segmentTemplate을 찾을 수 없습니다")?;

    let media_template = seg_template
        .get("media")
        .and_then(|v| v.as_str())
        .ok_or("media template을 찾을 수 없습니다")?;

    let timescale = seg_template
        .get("timescale")
        .and_then(|v| v.as_u64())
        .unwrap_or(1000) as f64;

    let timeline = seg_template
        .get("segmentTimeline")
        .and_then(|t| t.get("s"))
        .and_then(|s| s.as_array())
        .ok_or("segmentTimeline을 찾을 수 없습니다")?;

    // 시작/종료 시간을 초 단위로 변환
    let s_limit = time_to_sec(start_time);
    let e_limit = if end_time.is_empty() {
        f64::MAX
    } else {
        time_to_sec(end_time)
    };

    let mut segment_urls = Vec::new();
    let mut seg_number = 1u32;
    let mut curr_time = 0.0;

    for seg in timeline {
        let duration = seg
            .get("d")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as f64
            / timescale;

        let repeat = seg.get("r").and_then(|v| v.as_i64()).unwrap_or(0);

        let count = if repeat >= 0 { repeat + 1 } else { 1 } as u32;

        for _ in 0..count {
            if curr_time + duration >= s_limit && curr_time <= e_limit {
                let url = media_template
                    .replace("$RepresentationID$", rep_id)
                    .replace("$Number%06d$", &format!("{:06}", seg_number))
                    .replace("$Number$", &seg_number.to_string());

                segment_urls.push(format!("{}{}", base_url, url));
            }

            curr_time += duration;
            seg_number += 1;

            if curr_time > e_limit {
                break;
            }
        }

        if curr_time > e_limit {
            break;
        }
    }

    Ok(segment_urls)
}

pub async fn parse_segments(
    master_url: &str,
    start_time: &str,
    end_time: &str,
    quality_id: Option<&str>,
) -> Result<Vec<String>, String> {
    let client = build_client();

    let master_text = client
        .get(master_url)
        .send()
        .await
        .map_err(|e| format!("Master playlist 요청 실패: {}", e))?
        .text()
        .await
        .map_err(|e| format!("Master playlist 읽기 실패: {}", e))?;

    // 화질 선택: quality_id가 있으면 해당 variant 사용, 없으면 최고 화질
    let quality_path = if let Some(qid) = quality_id {
        // 지정된 variant playlist URL 사용
        qid
    } else {
        // 최고 화질 (마지막 variant) 선택
        master_text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .last()
            .ok_or("Quality playlist를 찾을 수 없습니다")?
    };

    let quality_url = if quality_path.starts_with("http://") || quality_path.starts_with("https://") {
        quality_path.to_string()
    } else {
        resolve_url(master_url, quality_path)
    };

    let playlist_text = client
        .get(&quality_url)
        .send()
        .await
        .map_err(|e| format!("Quality playlist 요청 실패: {}", e))?
        .text()
        .await
        .map_err(|e| format!("Quality playlist 읽기 실패: {}", e))?;

    let mut segment_urls: Vec<String> = Vec::new();

    let map_re = Regex::new(r#"#EXT-X-MAP:URI="([^"]+)""#).unwrap();
    if let Some(cap) = map_re.captures(&playlist_text) {
        segment_urls.push(resolve_url(&quality_url, &cap[1]));
    }

    let lines: Vec<&str> = playlist_text.lines().collect();
    let extinf_re = Regex::new(r"([\d.]+)").unwrap();

    let total_duration: f64 = lines
        .iter()
        .filter(|l| l.starts_with("#EXTINF"))
        .filter_map(|l| extinf_re.find(l))
        .filter_map(|m| m.as_str().parse::<f64>().ok())
        .sum();

    let s_limit = time_to_sec(start_time);
    let e_limit = if end_time.is_empty() {
        total_duration
    } else {
        time_to_sec(end_time)
    };

    let mut curr_time: f64 = 0.0;

    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("#EXTINF") {
            if let Some(m) = extinf_re.find(line) {
                if let Ok(dur) = m.as_str().parse::<f64>() {
                    if curr_time + dur >= s_limit && curr_time <= e_limit {
                        if i + 1 < lines.len() {
                            let seg_line = lines[i + 1].trim();
                            if !seg_line.starts_with('#') {
                                segment_urls.push(resolve_url(&quality_url, seg_line));
                            }
                        }
                    }
                    curr_time += dur;
                    if curr_time > e_limit {
                        break;
                    }
                }
            }
        }
    }

    Ok(segment_urls)
}

pub async fn download_segments(
    app: &AppHandle,
    segment_urls: &[String],
    temp_dir: &Path,
) -> Result<(), String> {
    fs::create_dir_all(temp_dir)
        .await
        .map_err(|e| format!("임시 폴더 생성 실패: {}", e))?;

    let client = build_client();
    let total = segment_urls.len() as u32;
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));

    let results: Vec<Result<(), String>> = stream::iter(segment_urls.iter().cloned().enumerate())
        .map(|(idx, url)| {
            let client = client.clone();
            let temp_dir = temp_dir.to_path_buf();
            let counter = counter.clone();
            let app = app.clone();
            let total = total;

            async move {
                let target_path = temp_dir.join(format!("seg_{:05}.m4s", idx));

                if target_path.exists() {
                    let done =
                        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    let _ = app.emit(
                        "download-progress",
                        DownloadProgress {
                            stage: "downloading".into(),
                            current: done,
                            total,
                            message: format!("세그먼트 다운로드 중... ({}/{})", done, total),
                        },
                    );
                    return Ok(());
                }

                let resp = client
                    .get(&url)
                    .timeout(std::time::Duration::from_secs(30))
                    .send()
                    .await
                    .map_err(|e| format!("세그먼트 {} 다운로드 실패: {}", idx, e))?;

                let bytes = resp
                    .bytes()
                    .await
                    .map_err(|e| format!("세그먼트 {} 읽기 실패: {}", idx, e))?;

                let mut file = fs::File::create(&target_path)
                    .await
                    .map_err(|e| format!("파일 생성 실패: {}", e))?;

                file.write_all(&bytes)
                    .await
                    .map_err(|e| format!("파일 쓰기 실패: {}", e))?;

                let done =
                    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                let _ = app.emit(
                    "download-progress",
                    DownloadProgress {
                        stage: "downloading".into(),
                        current: done,
                        total,
                        message: format!("세그먼트 다운로드 중... ({}/{})", done, total),
                    },
                );

                Ok(())
            }
        })
        .buffer_unordered(20)
        .collect()
        .await;

    for r in results {
        r?;
    }

    Ok(())
}

pub async fn merge_segments(
    app: &AppHandle,
    segment_count: usize,
    temp_dir: &Path,
) -> Result<PathBuf, String> {
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            stage: "merging".into(),
            current: 0,
            total: 1,
            message: "세그먼트 병합 중...".into(),
        },
    );

    let combined_path = temp_dir.join("combined.raw");
    let mut outfile = fs::File::create(&combined_path)
        .await
        .map_err(|e| format!("병합 파일 생성 실패: {}", e))?;

    for i in 0..segment_count {
        let seg_path = temp_dir.join(format!("seg_{:05}.m4s", i));
        if seg_path.exists() {
            let data = fs::read(&seg_path)
                .await
                .map_err(|e| format!("세그먼트 읽기 실패: {}", e))?;
            outfile
                .write_all(&data)
                .await
                .map_err(|e| format!("병합 쓰기 실패: {}", e))?;
        }
    }

    Ok(combined_path)
}

pub fn build_output_filename(
    info: &VideoInfo,
    start_time: &str,
    end_time: &str,
    output_dir: &str,
) -> PathBuf {
    let safe_channel = sanitize_filename(&info.channel);
    let safe_title = sanitize_filename(&info.title);
    let s_tag = start_time.replace(':', "");
    let e_tag = if end_time.is_empty() {
        "END".to_string()
    } else {
        end_time.replace(':', "")
    };

    let filename = format!("{}_{}_{}_{}.mp4", safe_channel, safe_title, s_tag, e_tag);
    Path::new(output_dir).join(filename)
}

pub async fn remux_with_ffmpeg(
    app: &AppHandle,
    ffmpeg_path: &Path,
    combined_path: &Path,
    output_path: &Path,
) -> Result<(), String> {
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            stage: "remuxing".into(),
            current: 0,
            total: 1,
            message: "ffmpeg로 리먹싱 중...".into(),
        },
    );

    let output = tokio::process::Command::new(ffmpeg_path)
        .args([
            "-y",
            "-i",
            combined_path.to_str().unwrap(),
            "-c",
            "copy",
            "-map",
            "0",
            "-movflags",
            "faststart",
            "-bsf:a",
            "aac_adtstoasc",
            output_path.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("ffmpeg 실행 실패: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ffmpeg 오류 (코드 {:?}): {}",
            output.status.code(),
            stderr
        ));
    }

    Ok(())
}

pub async fn cleanup_temp(temp_dir: &Path) -> Result<(), String> {
    fs::remove_dir_all(temp_dir)
        .await
        .map_err(|e| format!("임시 파일 정리 실패: {}", e))?;
    Ok(())
}
