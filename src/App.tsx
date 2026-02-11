import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  Link,
  Clock,
  FolderOpen,
  Download,
  Film,
  Minus,
  Square,
  X,
  Loader2,
  AlertTriangle,
  CheckCircle2,
  Settings,
  LogIn,
  Cookie,
} from "lucide-react";

// ── Types ──────────────────────────────────────────────

interface DownloadProgress {
  stage: string;
  current: number;
  total: number;
  message: string;
}

interface VideoQuality {
  id: string;
  width: number;
  height: number;
  bandwidth: number;
  label: string;
}

interface VodInfo {
  title: string;
  channel: string;
  duration: number;
  thumbnail: string;
  qualities: VideoQuality[];
}

interface ClipInfoResp {
  title: string;
  channel: string;
  thumbnail: string;
}

// 통합 미리보기 정보
interface PreviewInfo {
  title: string;
  channel: string;
  thumbnail: string;
  duration?: number;
}

type ParsedInput =
  | { type: "video"; id: string }
  | { type: "clip"; id: string }
  | null;

// ── Helpers ────────────────────────────────────────────

function secondsToHms(sec: number): string {
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const s = sec % 60;
  return [h, m, s].map((v) => String(v).padStart(2, "0")).join(":");
}

function hmsToSeconds(hms: string): number {
  if (!hms || !hms.includes(":")) return 0;
  const parts = hms.split(":").map((p) => parseInt(p) || 0);
  return parts[0] * 3600 + parts[1] * 60 + parts[2];
}

// 시간 입력을 위한 유틸리티
function parseTimeString(value: string): [string, string, string] {
  const parts = value.split(":");
  return [
    (parts[0] || "00").padStart(2, "0"),
    (parts[1] || "00").padStart(2, "0"),
    (parts[2] || "00").padStart(2, "0"),
  ];
}

function handleTimeInput(
  currentValue: string,
  key: string,
  cursorPos: number,
): string {
  if (!/^\d$/.test(key)) return currentValue;

  const [hh, mm, ss] = parseTimeString(currentValue);
  let field: "hours" | "minutes" | "seconds";

  if (cursorPos <= 2) field = "hours";
  else if (cursorPos <= 5) field = "minutes";
  else field = "seconds";

  let newHH = hh;
  let newMM = mm;
  let newSS = ss;

  // 기존 일의 자리가 십의 자리로 shift, 새 입력이 일의 자리로
  if (field === "hours") {
    newHH = (hh.charAt(1) + key).padStart(2, "0");
  } else if (field === "minutes") {
    newMM = (mm.charAt(1) + key).padStart(2, "0");
  } else {
    newSS = (ss.charAt(1) + key).padStart(2, "0");
  }

  return `${newHH}:${newMM}:${newSS}`;
}

function parseInput(input: string): ParsedInput {
  const trimmed = input.trim();
  if (!trimmed) return null;

  const clipMatch = trimmed.match(/chzzk\.naver\.com\/clips\/([^/?]+)/);
  if (clipMatch) return { type: "clip", id: clipMatch[1] };

  const videoMatch = trimmed.match(/chzzk\.naver\.com\/video\/(\d+)/);
  if (videoMatch) return { type: "video", id: videoMatch[1] };

  if (/^\d+$/.test(trimmed)) return { type: "video", id: trimmed };
  if (/^[a-zA-Z0-9]+$/.test(trimmed)) return { type: "clip", id: trimmed };

  return null;
}

// ── Reusable Styles ────────────────────────────────────

const card =
  "bg-white/[0.03] backdrop-blur-xl border border-white/[0.07] rounded-2xl";

const inputBase =
  "w-full px-4 py-2.5 bg-white/[0.04] border border-white/10 rounded-xl text-white text-sm placeholder-white/20 outline-none transition-all duration-200 focus:ring-2 focus:ring-chzzk/30 focus:border-chzzk/30 disabled:opacity-40";

// ── Window Handle ──────────────────────────────────────

const appWindow = getCurrentWindow();

// ── Component ──────────────────────────────────────────

function App() {
  const [videoInput, setVideoInput] = useState("");
  const [startTime, setStartTime] = useState("00:00:00");
  const [endTime, setEndTime] = useState("");
  const [outputDir, setOutputDir] = useState("");
  const [isDownloading, setIsDownloading] = useState(false);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [toast, setToast] = useState<ToastData | null>(null);

  const showToast = useCallback(
    (type: "success" | "error", message: string, detail?: string) => {
      setToast({ type, message, detail });
    },
    [],
  );

  const [preview, setPreview] = useState<PreviewInfo | null>(null);
  const [fetchingInfo, setFetchingInfo] = useState(false);
  const [availableQualities, setAvailableQualities] = useState<VideoQuality[]>([]);
  const [selectedQuality, setSelectedQuality] = useState<string>("auto");

  const [ffmpegReady, setFfmpegReady] = useState<boolean | null>(null);
  const [installingFfmpeg, setInstallingFfmpeg] = useState(false);

  const [showSettings, setShowSettings] = useState(false);
  const [authMethod, setAuthMethod] = useState<"login" | "cookie">("cookie");
  const [nidAuth, setNidAuth] = useState("");
  const [nidSession, setNidSession] = useState("");
  const [loginWebviewOpen, setLoginWebviewOpen] = useState(false);

  const parsed = parseInput(videoInput);
  const isClip = parsed?.type === "clip";

  // ── Effects ────────────────────────────────────────

  useEffect(() => {
    invoke<boolean>("check_ffmpeg").then(setFfmpegReady);
  }, []);

  useEffect(() => {
    // 저장된 쿠키 불러오기
    invoke<{ nid_aut: string; nid_ses: string } | null>("load_credentials")
      .then((creds) => {
        if (creds) {
          setNidAuth(creds.nid_aut);
          setNidSession(creds.nid_ses);
        }
      })
      .catch(console.error);

    // 로그인 성공 이벤트 리스너
    const unlisten = listen<{ nid_aut: string; nid_ses: string }>(
      "login-success",
      async (event) => {
        const creds = event.payload;
        setNidAuth(creds.nid_aut);
        setNidSession(creds.nid_ses);
        setLoginWebviewOpen(false);
        setShowSettings(false);
        showToast("success", "로그인 성공! 인증 정보가 저장되었습니다");
      }
    );

    return () => {
      unlisten.then((fn) => fn()).catch(console.error);
    };
  }, []);

  useEffect(() => {
    const unlisten = listen<DownloadProgress>("download-progress", (e) =>
      setProgress(e.payload),
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    if (!parsed) {
      setPreview(null);
      return;
    }
    console.log("🔍 Parsed input:", parsed);
    const timer = setTimeout(() => {
      setFetchingInfo(true);
      if (parsed.type === "video") {
        invoke<VodInfo>("fetch_video_info", { videoId: parsed.id })
          .then((info) => {
            console.log("✅ Video info loaded:", info);
            setPreview({
              title: info.title,
              channel: info.channel,
              thumbnail: info.thumbnail,
              duration: info.duration,
            });
            if (info.duration > 0) setEndTime(secondsToHms(info.duration));

            // 화질 목록 저장 및 정렬
            if (info.qualities && info.qualities.length > 0) {
              // bandwidth 기준 내림차순 정렬 (최고 화질 먼저)
              const sortedQualities = [...info.qualities].sort(
                (a, b) => b.bandwidth - a.bandwidth
              );
              console.log("✅ Qualities sorted:", sortedQualities);
              setAvailableQualities(sortedQualities);
              setSelectedQuality("auto"); // 기본값: 자동 (최고 화질)
            } else {
              setAvailableQualities([]);
            }
          })
          .catch((err) => {
            console.error("❌ Failed to fetch video info:", err);
            setPreview(null);
            setAvailableQualities([]);
          })
          .finally(() => setFetchingInfo(false));
      } else {
        invoke<ClipInfoResp>("fetch_clip_info", { clipUid: parsed.id })
          .then((info) => {
            console.log("✅ Clip info loaded:", info);
            setPreview({
              title: info.title,
              channel: info.channel,
              thumbnail: info.thumbnail,
            });
          })
          .catch((err) => {
            console.error("❌ Failed to fetch clip info:", err);
            setPreview(null);
          })
          .finally(() => setFetchingInfo(false));
      }
    }, 500);
    return () => clearTimeout(timer);
  }, [parsed?.id, parsed?.type]);

  // ── 시간 검증 ──────────────────────────────────────

  // 종료 시간이 영상 길이를 초과하면 자동으로 영상 길이로 설정
  useEffect(() => {
    if (!preview?.duration || !endTime || isClip) return;

    const endSec = hmsToSeconds(endTime);
    if (endSec > preview.duration) {
      setEndTime(secondsToHms(preview.duration));
    }
  }, [endTime, preview?.duration, isClip]);

  // 시작 시간이 영상 길이를 초과하면 자동으로 보정
  useEffect(() => {
    if (!preview?.duration || !startTime || isClip) return;

    const startSec = hmsToSeconds(startTime);
    if (startSec >= preview.duration) {
      const maxStart = Math.max(0, preview.duration - 60); // 최소 60초 여유
      setStartTime(secondsToHms(maxStart));
    }
  }, [startTime, preview?.duration, isClip]);

  // 시간 범위 유효성 검사
  const timeRangeError = (() => {
    if (isClip || !preview?.duration) return null;

    const startSec = hmsToSeconds(startTime);
    const endSec = endTime ? hmsToSeconds(endTime) : preview.duration;

    if (startSec >= endSec) {
      return "시작 시간이 종료 시간보다 크거나 같습니다";
    }

    return null;
  })();

  // ── Handlers ───────────────────────────────────────

  const handleInstallFfmpeg = async () => {
    setInstallingFfmpeg(true);
    setToast(null);
    setProgress(null);
    try {
      await invoke<string>("install_ffmpeg");
      setFfmpegReady(true);
    } catch (e) {
      showToast("error", "ffmpeg 설치 실패", String(e));
    } finally {
      setInstallingFfmpeg(false);
    }
  };

  const selectOutputDir = async () => {
    const selected = await open({ directory: true });
    if (selected) setOutputDir(selected as string);
  };

  const handleDownload = async () => {
    if (!parsed) {
      showToast("error", "Video ID 또는 Clip URL을 입력해주세요.");
      return;
    }
    if (!outputDir) {
      showToast("error", "저장 경로를 선택해주세요.");
      return;
    }

    setIsDownloading(true);
    setToast(null);
    setProgress(null);

    try {
      let outputPath: string;
      if (parsed.type === "clip") {
        outputPath = await invoke<string>("download_clip_cmd", {
          clipUid: parsed.id,
          outputDir,
        });
      } else {
        outputPath = await invoke<string>("download_vod", {
          videoId: parsed.id,
          startTime: startTime || "00:00:00",
          endTime: endTime || "",
          outputDir,
          qualityId: selectedQuality === "auto" ? null : selectedQuality,
        });
      }
      showToast("success", "다운로드 완료!", outputPath);
    } catch (e) {
      showToast("error", "다운로드 실패", String(e));
    } finally {
      setIsDownloading(false);
    }
  };

  // ── Derived ────────────────────────────────────────

  const isBusy = isDownloading || installingFfmpeg;
  const needsFfmpeg = !isClip && ffmpegReady === false;
  const progressPercent =
    progress && progress.total > 0
      ? Math.round((progress.current / progress.total) * 100)
      : 0;

  // ── Render ─────────────────────────────────────────

  return (
    <div className="h-screen bg-[#09090b] text-white flex flex-col overflow-hidden relative">
      {/* Background ambient glow */}
      <div className="pointer-events-none fixed inset-0 bg-[radial-gradient(ellipse_at_top,rgba(0,255,163,0.035)_0%,transparent_55%)]" />

      {/* ── Titlebar ────────────────────────────────── */}
      <div
        data-tauri-drag-region
        className="h-9 flex items-center justify-between px-4 select-none shrink-0 relative z-10"
      >
        <span className="text-[11px] font-semibold tracking-wide text-white/25 pointer-events-none">
          CHZZK DOWNLOADER
        </span>

        <div className="flex items-center gap-0.5">
          <button
            onClick={() => setShowSettings(true)}
            className="w-8 h-7 flex items-center justify-center text-white/30 hover:text-white hover:bg-white/10 rounded-lg transition-colors pointer-events-auto"
          >
            <Settings size={14} />
          </button>
          <button
            onClick={() => appWindow.minimize()}
            className="w-8 h-7 flex items-center justify-center text-white/30 hover:text-white hover:bg-white/10 rounded-lg transition-colors"
          >
            <Minus size={14} />
          </button>
          <button
            onClick={() => appWindow.toggleMaximize()}
            className="w-8 h-7 flex items-center justify-center text-white/30 hover:text-white hover:bg-white/10 rounded-lg transition-colors"
          >
            <Square size={10} />
          </button>
          <button
            onClick={() => appWindow.close()}
            className="w-8 h-7 flex items-center justify-center text-white/30 hover:text-white hover:bg-red-500/80 rounded-lg transition-colors"
          >
            <X size={14} />
          </button>
        </div>
      </div>

      {/* ── Content ─────────────────────────────────── */}
      <div className="flex-1 overflow-hidden px-5 pb-5 space-y-3 relative z-10">
        {/* FFmpeg banner */}
        {needsFfmpeg && !installingFfmpeg && (
          <div
            className={`${card} p-4 border-amber-500/20 bg-amber-500/[0.06] flex items-center justify-between`}
          >
            <div className="flex items-center gap-3">
              <AlertTriangle size={16} className="text-amber-400 shrink-0" />
              <span className="text-[13px] text-amber-300/90">
                VOD 다운로드에 ffmpeg가 필요합니다
              </span>
            </div>
            <button
              onClick={handleInstallFfmpeg}
              className="px-4 py-1.5 bg-amber-500/15 hover:bg-amber-500/25 border border-amber-500/25 rounded-xl text-xs font-medium text-amber-300 transition-all"
            >
              설치
            </button>
          </div>
        )}

        {/* FFmpeg install progress */}
        {installingFfmpeg && progress?.stage === "ffmpeg-install" && (
          <div className={`${card} p-4 space-y-3`}>
            <ProgressBar percent={progressPercent} />
            <p className="text-xs text-white/40 text-center">
              {progress.message}
            </p>
          </div>
        )}

        {/* ── Preview Card ───────────────────────────── */}
        <div className={`${card} p-4 overflow-hidden`}>
          <div className="flex items-center gap-2 mb-3">
            <div className="w-6 h-6 flex items-center justify-center rounded-md bg-chzzk/10">
              <Film size={12} className="text-chzzk" />
            </div>
            <span className="text-[13px] font-medium text-white/60">
              {isClip ? "클립 정보" : "영상 정보"}
            </span>
          </div>
            {fetchingInfo ? (
              <div className="flex items-center justify-center gap-2 py-2">
                <Loader2 size={14} className="text-white/25 animate-spin" />
                <span className="text-xs text-white/25">
                  정보를 가져오는 중...
                </span>
              </div>
            ) : preview ? (
                <div className="flex gap-4 items-center">
                  {preview.thumbnail ? (
                    <img
                      src={preview.thumbnail}
                      alt=""
                      className="w-28 rounded-xl shrink-0 border border-white/10"
                    />
                  ) : (
                    <div className="w-28 h-16 rounded-xl bg-white/[0.04] border border-white/10 shrink-0 flex items-center justify-center">
                      <Download size={16} className="text-white/15" />
                    </div>
                  )}
                  <div className="min-w-0 flex-1">
                    <p className="text-[13px] text-white/80 font-medium leading-snug line-clamp-2">
                      {preview.title}
                    </p>
                    <p className="text-xs text-white/30 mt-1 truncate">
                      {preview.channel}
                      {preview.duration != null &&
                        ` · ${secondsToHms(preview.duration)}`}
                    </p>
                  </div>
                </div>
              ) : (
                <div className="flex gap-4 items-center py-2">
                  <div className="w-28 h-16 rounded-xl bg-white/[0.04] border border-white/10 border-dashed shrink-0 flex items-center justify-center">
                    <Film size={16} className="text-white/15" />
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="text-[13px] text-white/25 leading-snug">
                      URL을 입력하세요
                    </p>
                  </div>
                </div>
              )}
        </div>

        {/* ── URL Section ───────────────────────────── */}
        <div className={`${card} p-5`}>
          <div className="flex items-center gap-2.5 mb-3">
            <div className="w-7 h-7 flex items-center justify-center rounded-lg bg-chzzk/10">
              <Link size={14} className="text-chzzk" />
            </div>
            <label className="text-sm font-medium text-white/60">
              Video / Clip URL
            </label>
            {parsed && (
              <span className="ml-auto px-2.5 py-0.5 text-[10px] font-bold rounded-md bg-chzzk/10 text-chzzk tracking-widest">
                {parsed.type === "clip" ? "CLIP" : "VOD"}
              </span>
            )}
          </div>

          <input
            type="text"
            placeholder="chzzk.naver.com/video/... 또는 /clips/..."
            value={videoInput}
            onChange={(e) => setVideoInput(e.target.value)}
            disabled={isBusy}
            className={`${inputBase} py-3`}
          />
        </div>

        {/* ── Bento Grid ────────────────────────────── */}
        <div className="grid grid-cols-2 gap-3">
          {/* Time Card */}
          {!isClip && (
            <div className={`${card} p-4`}>
              <div className="flex items-center gap-2 mb-3">
                <div className="w-6 h-6 flex items-center justify-center rounded-md bg-chzzk/10">
                  <Clock size={12} className="text-chzzk" />
                </div>
                <span className="text-[13px] font-medium text-white/60">
                  시간 설정
                </span>
              </div>

              <div className="space-y-2">
                <div>
                  <label className="text-[11px] text-white/25 mb-1 block">
                    시작
                  </label>
                  <input
                    type="text"
                    placeholder="00:00:00"
                    value={startTime}
                    onChange={(e) => {
                      const val = e.target.value;
                      if (val.length <= 8) setStartTime(val);
                    }}
                    onKeyDown={(e) => {
                      if (e.key >= "0" && e.key <= "9") {
                        e.preventDefault();
                        const input = e.currentTarget;
                        const cursorPos = input.selectionStart || 0;
                        const newValue = handleTimeInput(
                          startTime,
                          e.key,
                          cursorPos,
                        );
                        setStartTime(newValue);

                        // 자동으로 다음 위치로 커서 이동
                        setTimeout(() => {
                          let nextPos = cursorPos;
                          if (cursorPos === 0) nextPos = 1;       // HH 첫자리 → 둘째자리
                          else if (cursorPos === 1) nextPos = 3;  // HH 둘째자리 → MM 시작
                          else if (cursorPos === 2) nextPos = 3;  // 콜론 위치 → MM 시작
                          else if (cursorPos === 3) nextPos = 4;  // MM 첫자리 → 둘째자리
                          else if (cursorPos === 4) nextPos = 6;  // MM 둘째자리 → SS 시작
                          else if (cursorPos === 5) nextPos = 6;  // 콜론 위치 → SS 시작
                          else if (cursorPos === 6) nextPos = 7;  // SS 첫자리 → 둘째자리
                          else if (cursorPos === 7) nextPos = 8;  // SS 둘째자리 → 끝
                          else nextPos = 8;                        // 끝 유지

                          input.setSelectionRange(nextPos, nextPos);
                        }, 0);
                      } else if (e.key === "Backspace") {
                        e.preventDefault();
                        const input = e.currentTarget;
                        const cursorPos = input.selectionStart || 0;
                        const [hh, mm, ss] = parseTimeString(startTime);
                        let newHH = hh,
                          newMM = mm,
                          newSS = ss;

                        if (cursorPos <= 2) newHH = "0" + hh.slice(0, 1);
                        else if (cursorPos <= 5) newMM = "0" + mm.slice(0, 1);
                        else newSS = "0" + ss.slice(0, 1);

                        setStartTime(`${newHH}:${newMM}:${newSS}`);
                        setTimeout(() => input.setSelectionRange(cursorPos, cursorPos), 0);
                      }
                    }}
                    onClick={(e) => {
                      const input = e.currentTarget;
                      const clickPos = input.selectionStart || 0;
                      const [hh, mm, ss] = parseTimeString(startTime);

                      let newHH = hh, newMM = mm, newSS = ss;
                      let cursorPos = clickPos;

                      // 클릭한 필드 초기화
                      if (clickPos <= 2) {
                        newHH = "00";
                        cursorPos = 0;
                      } else if (clickPos <= 5) {
                        newMM = "00";
                        cursorPos = 3;
                      } else {
                        newSS = "00";
                        cursorPos = 6;
                      }

                      setStartTime(`${newHH}:${newMM}:${newSS}`);
                      setTimeout(() => input.setSelectionRange(cursorPos, cursorPos), 0);
                    }}
                    onFocus={() => {
                      if (!startTime.includes(":")) {
                        setStartTime("00:00:00");
                      }
                    }}
                    disabled={isBusy}
                    className={`${inputBase} font-mono text-[13px]`}
                  />
                </div>
                <div>
                  <label className="text-[11px] text-white/25 mb-1 block">
                    종료
                  </label>
                  <input
                    type="text"
                    placeholder="비우면 끝까지"
                    value={endTime}
                    onChange={(e) => {
                      const val = e.target.value;
                      if (val.length <= 8) setEndTime(val);
                    }}
                    onKeyDown={(e) => {
                      if (e.key >= "0" && e.key <= "9") {
                        e.preventDefault();
                        const input = e.currentTarget;
                        const cursorPos = input.selectionStart || 0;
                        const newValue = handleTimeInput(
                          endTime || "00:00:00",
                          e.key,
                          cursorPos,
                        );
                        setEndTime(newValue);

                        // 자동으로 다음 위치로 커서 이동
                        setTimeout(() => {
                          let nextPos = cursorPos;
                          if (cursorPos === 0) nextPos = 1;       // HH 첫자리 → 둘째자리
                          else if (cursorPos === 1) nextPos = 3;  // HH 둘째자리 → MM 시작
                          else if (cursorPos === 2) nextPos = 3;  // 콜론 위치 → MM 시작
                          else if (cursorPos === 3) nextPos = 4;  // MM 첫자리 → 둘째자리
                          else if (cursorPos === 4) nextPos = 6;  // MM 둘째자리 → SS 시작
                          else if (cursorPos === 5) nextPos = 6;  // 콜론 위치 → SS 시작
                          else if (cursorPos === 6) nextPos = 7;  // SS 첫자리 → 둘째자리
                          else if (cursorPos === 7) nextPos = 8;  // SS 둘째자리 → 끝
                          else nextPos = 8;                        // 끝 유지

                          input.setSelectionRange(nextPos, nextPos);
                        }, 0);
                      } else if (e.key === "Backspace") {
                        e.preventDefault();
                        const input = e.currentTarget;
                        const cursorPos = input.selectionStart || 0;
                        const [hh, mm, ss] = parseTimeString(endTime || "00:00:00");
                        let newHH = hh,
                          newMM = mm,
                          newSS = ss;

                        if (cursorPos <= 2) newHH = "0" + hh.slice(0, 1);
                        else if (cursorPos <= 5) newMM = "0" + mm.slice(0, 1);
                        else newSS = "0" + ss.slice(0, 1);

                        setEndTime(`${newHH}:${newMM}:${newSS}`);
                        setTimeout(() => input.setSelectionRange(cursorPos, cursorPos), 0);
                      }
                    }}
                    onClick={(e) => {
                      const input = e.currentTarget;
                      const clickPos = input.selectionStart || 0;
                      const [hh, mm, ss] = parseTimeString(endTime || "00:00:00");

                      let newHH = hh, newMM = mm, newSS = ss;
                      let cursorPos = clickPos;

                      // 클릭한 필드 초기화
                      if (clickPos <= 2) {
                        newHH = "00";
                        cursorPos = 0;
                      } else if (clickPos <= 5) {
                        newMM = "00";
                        cursorPos = 3;
                      } else {
                        newSS = "00";
                        cursorPos = 6;
                      }

                      setEndTime(`${newHH}:${newMM}:${newSS}`);
                      setTimeout(() => input.setSelectionRange(cursorPos, cursorPos), 0);
                    }}
                    onFocus={() => {
                      if (!endTime || !endTime.includes(":")) {
                        setEndTime("00:00:00");
                      }
                    }}
                    disabled={isBusy}
                    className={`${inputBase} font-mono text-[13px]`}
                  />
                </div>
              </div>
            </div>
          )}

          {/* Quality Card */}
          {!isClip && (
            <div className={`${card} p-4`}>
              <div className="flex items-center gap-2 mb-3">
                <div className="w-6 h-6 flex items-center justify-center rounded-md bg-chzzk/10">
                  <Film size={12} className="text-chzzk" />
                </div>
                <span className="text-[13px] font-medium text-white/60">
                  화질 선택
                </span>
              </div>

              {availableQualities.length > 0 ? (
                <>
              {/* 실제 다운로드 시간 계산 (시작~종료) */}
              {(() => {
                const startSec = hmsToSeconds(startTime);
                const endSec = endTime
                  ? hmsToSeconds(endTime)
                  : preview?.duration || 0;
                const downloadDuration = Math.max(0, endSec - startSec);

                return (
                  <div className="grid grid-cols-2 gap-2 max-h-[140px] overflow-y-auto custom-scrollbar">
                    {/* 자동 (최고 화질) */}
                    <button
                  onClick={() => setSelectedQuality("auto")}
                  disabled={isBusy}
                  className={`px-3 py-2.5 rounded-xl text-xs font-medium transition-all duration-200 ${
                    selectedQuality === "auto"
                      ? "bg-chzzk/15 border-2 border-chzzk/40 text-chzzk ring-2 ring-chzzk/20"
                      : "bg-white/[0.04] border border-white/10 text-white/60 hover:bg-white/[0.08] hover:text-white/80"
                  } disabled:opacity-40 disabled:cursor-not-allowed`}
                >
                  <div className="text-center">
                    <div className="font-bold">자동</div>
                    <div className="text-[10px] opacity-60 mt-0.5">최고 화질</div>
                  </div>
                </button>

                    {/* 사용 가능한 화질 목록 */}
                    {availableQualities.map((quality) => {
                      // 예상 파일 크기 계산 (시작~종료 시간 기준)
                      const estimatedSizeMB = downloadDuration
                        ? Math.round(
                            (quality.bandwidth * downloadDuration) / 8 / 1_000_000
                          )
                        : null;

                  // 용량 단위 포맷 (1000MB 이상이면 GB로)
                  const formatSize = (mb: number | null) => {
                    if (mb === null) return null;
                    if (mb >= 1000) {
                      return `~${(mb / 1000).toFixed(1)}GB`;
                    }
                    return `~${mb}MB`;
                  };

                  // 디버깅
                  if (quality === availableQualities[0]) {
                    console.log("📊 Size calculation:", {
                      bandwidth: quality.bandwidth,
                      duration: preview?.duration,
                      estimated: estimatedSizeMB,
                    });
                  }

                  return (
                    <button
                      key={quality.id}
                      onClick={() => setSelectedQuality(quality.id)}
                      disabled={isBusy}
                      className={`px-3 py-2.5 rounded-xl text-xs font-medium transition-all duration-200 ${
                        selectedQuality === quality.id
                          ? "bg-chzzk/15 border-2 border-chzzk/40 text-chzzk ring-2 ring-chzzk/20"
                          : "bg-white/[0.04] border border-white/10 text-white/60 hover:bg-white/[0.08] hover:text-white/80"
                      } disabled:opacity-40 disabled:cursor-not-allowed`}
                    >
                      <div className="text-center">
                        <div className="font-bold">{quality.height}p</div>
                        <div className="text-[10px] opacity-60 mt-0.5 leading-tight">
                          {formatSize(estimatedSizeMB) ||
                            `${(quality.bandwidth / 1_000_000).toFixed(1)}Mbps`}
                        </div>
                      </div>
                    </button>
                      );
                    })}
                  </div>
                );
              })()}
                </>
              ) : (
                <div className="flex items-center justify-center py-4">
                  <p className="text-[13px] text-white/25">
                    영상 정보를 불러오면 화질 옵션이 표시됩니다
                  </p>
                </div>
              )}
            </div>
          )}
        </div>

        {/* ── 저장 경로 (얇게) ──────────────────────────── */}
        <div className={`${card} px-4 py-3`}>
          <div className="flex items-center gap-3">
            <div className="w-5 h-5 flex items-center justify-center rounded-md bg-chzzk/10 shrink-0">
              <FolderOpen size={11} className="text-chzzk" />
            </div>
            <input
              type="text"
              value={outputDir}
              readOnly
              placeholder="저장 경로를 선택하세요"
              className="flex-1 min-w-0 bg-transparent text-xs text-white/60 truncate outline-none cursor-default"
            />
            <button
              onClick={selectOutputDir}
              disabled={isBusy}
              className="px-3 py-1.5 bg-white/[0.04] border border-white/10 rounded-lg text-[11px] text-white/50 hover:bg-white/[0.08] hover:text-white/80 transition-all disabled:opacity-40 disabled:cursor-not-allowed shrink-0"
            >
              찾아보기
            </button>
          </div>
        </div>

        {/* ── 시간 범위 경고 ───────────────────────── */}
        {timeRangeError && (
          <div className="group relative overflow-hidden">
            {/* Ambient glow effect */}
            <div className="absolute -inset-1 bg-gradient-to-r from-red-500/20 via-orange-500/20 to-red-500/20 opacity-50 group-hover:opacity-75 blur-2xl transition-opacity duration-500" />

            {/* Main card */}
            <div className="relative px-4 py-3.5 bg-gradient-to-br from-red-500/[0.08] via-red-500/[0.06] to-orange-500/[0.08] border border-red-500/20 rounded-2xl backdrop-blur-xl transition-all duration-300 group-hover:border-red-400/30">
              <div className="flex items-start gap-3">
                {/* Icon */}
                <div className="mt-0.5 flex items-center justify-center w-7 h-7 rounded-xl bg-red-500/15 ring-1 ring-inset ring-red-500/25 group-hover:bg-red-500/20 transition-colors duration-300">
                  <AlertTriangle size={14} className="text-red-400" strokeWidth={2.5} />
                </div>

                {/* Message */}
                <div className="flex-1 min-w-0 pt-0.5">
                  <p className="text-[13px] font-medium text-red-300/95 leading-relaxed">
                    {timeRangeError}
                  </p>
                </div>
              </div>
            </div>
          </div>
        )}

        {/* ── Download Button ───────────────────────── */}
        <button
          onClick={handleDownload}
          disabled={isBusy || (needsFfmpeg && !isClip) || !!timeRangeError}
          className="w-full py-4 bg-chzzk text-[#09090b] font-bold text-[15px] rounded-2xl transition-all duration-300 hover:shadow-[0_0_50px_rgba(0,255,163,0.3)] active:scale-[0.98] disabled:opacity-25 disabled:cursor-not-allowed disabled:hover:shadow-none flex items-center justify-center gap-2.5 cursor-pointer"
        >
          {isDownloading ? (
            <>
              <Loader2 size={18} className="animate-spin" />
              다운로드 중...
            </>
          ) : (
            <>
              <Download size={18} strokeWidth={2.5} />
              다운로드 시작
            </>
          )}
        </button>

        {/* ── Progress ──────────────────────────────── */}
        {progress && !installingFfmpeg && (
          <div className="space-y-2.5">
            <ProgressBar percent={progressPercent} />
            <p className="text-xs text-white/35 text-center">
              {progress.message}
            </p>
          </div>
        )}

      </div>

      {/* ── Settings Modal ─────────────────────────── */}
      {showSettings && (
        <SettingsModal
          authMethod={authMethod}
          setAuthMethod={setAuthMethod}
          nidAuth={nidAuth}
          setNidAuth={setNidAuth}
          nidSession={nidSession}
          setNidSession={setNidSession}
          loginWebviewOpen={loginWebviewOpen}
          onClose={() => {
            setShowSettings(false);
            setLoginWebviewOpen(false);
          }}
          onSave={async () => {
            try {
              if (authMethod === "login") {
                // 로그인 창 열기 (자동으로 쿠키 감지하여 저장됨)
                await invoke("open_login_webview");
                setLoginWebviewOpen(true);
                showToast(
                  "success",
                  "로그인 창이 열렸습니다. 로그인하면 자동으로 저장됩니다."
                );
              } else {
                // 쿠키 직접 저장
                await invoke("save_credentials", {
                  nidAut: nidAuth,
                  nidSes: nidSession,
                });
                showToast("success", "인증 정보가 저장되었습니다");
                setShowSettings(false);
              }
            } catch (e) {
              showToast("error", "저장 실패", String(e));
              if (authMethod === "login") {
                setLoginWebviewOpen(false);
              }
            }
          }}
        />
      )}

      {/* ── Toast ──────────────────────────────────── */}
      {toast && <Toast toast={toast} onClose={() => setToast(null)} />}
    </div>
  );
}

// ── Sub-components ───────────────────────────────────────

function ProgressBar({ percent }: { percent: number }) {
  return (
    <div className="h-1.5 bg-white/[0.06] rounded-full overflow-hidden">
      <div
        className="h-full bg-chzzk rounded-full transition-all duration-300 shadow-[0_0_10px_rgba(0,255,163,0.5)]"
        style={{ width: `${percent}%` }}
      />
    </div>
  );
}

interface ToastData {
  type: "success" | "error";
  message: string;
  detail?: string;
}

function Toast({
  toast,
  onClose,
}: {
  toast: ToastData;
  onClose: () => void;
}) {
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    requestAnimationFrame(() => setVisible(true));
    const timer = setTimeout(() => {
      setVisible(false);
      setTimeout(onClose, 300);
    }, 5000);
    return () => clearTimeout(timer);
  }, [onClose]);

  const isSuccess = toast.type === "success";

  return (
    <div
      className={`
        fixed top-12 left-1/2 -translate-x-1/2 z-50 w-[calc(100%-40px)] max-w-[600px]
        transition-all duration-300 ease-out
        ${visible ? "opacity-100 translate-y-0" : "opacity-0 -translate-y-3"}
      `}
    >
      <div
        className={`
          rounded-2xl p-4 backdrop-blur-xl border shadow-lg
          ${
            isSuccess
              ? "bg-chzzk/[0.12] border-chzzk/20 shadow-chzzk/10"
              : "bg-red-500/[0.12] border-red-500/20 shadow-red-500/10"
          }
        `}
      >
        <div className="flex items-start gap-3">
          <div className="shrink-0 mt-0.5">
            {isSuccess ? (
              <CheckCircle2 size={16} className="text-chzzk" />
            ) : (
              <AlertTriangle size={16} className="text-red-400" />
            )}
          </div>
          <div className="flex-1 min-w-0">
            <p
              className={`text-[13px] font-semibold ${isSuccess ? "text-chzzk" : "text-red-400"}`}
            >
              {toast.message}
            </p>
            {toast.detail && (
              <p className="text-xs text-white/35 mt-1 break-all line-clamp-2">
                {toast.detail}
              </p>
            )}
          </div>
          <button
            onClick={() => {
              setVisible(false);
              setTimeout(onClose, 300);
            }}
            className="shrink-0 w-6 h-6 flex items-center justify-center rounded-lg text-white/30 hover:text-white/60 hover:bg-white/10 transition-colors"
          >
            <X size={12} />
          </button>
        </div>
      </div>
    </div>
  );
}

// ── Settings Modal ───────────────────────────────────────

interface SettingsModalProps {
  authMethod: "login" | "cookie";
  setAuthMethod: (method: "login" | "cookie") => void;
  nidAuth: string;
  setNidAuth: (auth: string) => void;
  nidSession: string;
  setNidSession: (session: string) => void;
  loginWebviewOpen: boolean;
  onClose: () => void;
  onSave: () => void;
}

function SettingsModal({
  authMethod,
  setAuthMethod,
  loginWebviewOpen,
  nidAuth,
  setNidAuth,
  nidSession,
  setNidSession,
  onClose,
  onSave,
}: SettingsModalProps) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="w-[500px] max-h-[80vh] overflow-y-auto custom-scrollbar">
        <div className={`${card} p-6`}>
          {/* Header */}
          <div className="flex items-center justify-between mb-6">
            <div className="flex items-center gap-3">
              <div className="w-8 h-8 flex items-center justify-center rounded-lg bg-chzzk/10">
                <Settings size={16} className="text-chzzk" />
              </div>
              <h2 className="text-lg font-bold text-white">설정</h2>
            </div>
            <button
              onClick={onClose}
              className="w-8 h-8 flex items-center justify-center rounded-lg text-white/30 hover:text-white hover:bg-white/10 transition-colors"
            >
              <X size={16} />
            </button>
          </div>

          {/* Auth Method Tabs */}
          <div className="flex gap-2 mb-6">
            <button
              onClick={() => setAuthMethod("cookie")}
              className={`flex-1 px-4 py-3 rounded-xl text-sm font-medium transition-all ${
                authMethod === "cookie"
                  ? "bg-chzzk/15 border-2 border-chzzk/40 text-chzzk"
                  : "bg-white/[0.04] border border-white/10 text-white/60 hover:bg-white/[0.08]"
              }`}
            >
              <Cookie size={14} className="inline mr-2" />
              쿠키 직접 입력
            </button>
            <button
              onClick={() => setAuthMethod("login")}
              className={`flex-1 px-4 py-3 rounded-xl text-sm font-medium transition-all ${
                authMethod === "login"
                  ? "bg-chzzk/15 border-2 border-chzzk/40 text-chzzk"
                  : "bg-white/[0.04] border border-white/10 text-white/60 hover:bg-white/[0.08]"
              }`}
            >
              <LogIn size={14} className="inline mr-2" />
              아이디/비밀번호
            </button>
          </div>

          {/* Cookie Input */}
          {authMethod === "cookie" && (
            <div className="space-y-4">
              <div>
                <label className="block text-xs text-white/40 mb-2">
                  NID_AUT 쿠키
                </label>
                <input
                  type="text"
                  value={nidAuth}
                  onChange={(e) => setNidAuth(e.target.value)}
                  placeholder="치지직에 로그인 후 개발자 도구에서 복사"
                  className={inputBase}
                />
              </div>
              <div>
                <label className="block text-xs text-white/40 mb-2">
                  NID_SES 쿠키
                </label>
                <input
                  type="text"
                  value={nidSession}
                  onChange={(e) => setNidSession(e.target.value)}
                  placeholder="치지직에 로그인 후 개발자 도구에서 복사"
                  className={inputBase}
                />
              </div>
              <div className="mt-4 p-3 bg-blue-500/10 border border-blue-500/20 rounded-xl">
                <p className="text-xs text-blue-300/80 leading-relaxed">
                  🍪 <strong>쿠키 가져오는 방법:</strong>
                  <br />
                  1. chzzk.naver.com에 로그인
                  <br />
                  2. F12 키로 개발자 도구 열기
                  <br />
                  3. Application → Cookies → https://chzzk.naver.com
                  <br />
                  4. NID_AUT와 NID_SES 값 복사
                </p>
              </div>
            </div>
          )}

          {/* Login Input */}
          {authMethod === "login" && (
            <div className="space-y-4">
              <div className="p-4 bg-chzzk/10 border border-chzzk/20 rounded-xl">
                <h3 className="text-sm font-bold text-chzzk mb-2">
                  웹 로그인 방식 (권장)
                </h3>
                <p className="text-xs text-white/60 leading-relaxed mb-3">
                  네이버 로그인 페이지가 새 창으로 열립니다. 로그인하면 자동으로
                  인증 정보가 저장됩니다.
                </p>
                <ol className="text-xs text-white/50 leading-relaxed space-y-1 list-decimal list-inside">
                  <li>"로그인 창 열기" 버튼 클릭</li>
                  <li>열린 창에서 네이버 로그인 완료</li>
                  <li>치지직 페이지로 이동하면 자동 저장</li>
                </ol>
              </div>
              {loginWebviewOpen && (
                <div className="p-3 bg-blue-500/10 border border-blue-500/20 rounded-xl animate-pulse">
                  <p className="text-xs text-blue-300/80 leading-relaxed">
                    🔄 로그인 대기 중... 로그인하면 자동으로 저장됩니다.
                  </p>
                </div>
              )}
            </div>
          )}

          {/* Action Buttons */}
          <div className="flex gap-3 mt-6">
            <button
              onClick={onClose}
              className="flex-1 px-4 py-3 bg-white/[0.04] border border-white/10 rounded-xl text-sm font-medium text-white/60 hover:bg-white/[0.08] hover:text-white/80 transition-all"
            >
              취소
            </button>
            <button
              onClick={onSave}
              className="flex-1 px-4 py-3 bg-chzzk text-[#09090b] rounded-xl text-sm font-bold hover:shadow-[0_0_30px_rgba(0,255,163,0.3)] transition-all"
            >
              {authMethod === "login" ? "로그인 창 열기" : "저장"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

export default App;
