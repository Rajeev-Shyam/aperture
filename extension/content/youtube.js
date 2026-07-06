// Aperture Capture Bridge — YouTube content script (ADR-027, Doc 10 §3).
//
// Reads video.currentTime — the primary, reliable position source (rung 1 of
// the capture hierarchy). Reports position-only snapshots to the service
// worker; never reads or forwards page content beyond the video id/URL
// (ADR-029). YouTube is an SPA, so URL changes arrive via yt-navigate-finish
// rather than full page loads.

const REPORT_INTERVAL_MS = 5000;

let intervalId = null;

function videoIdFromLocation() {
  try {
    const u = new URL(location.href);
    if (u.pathname === "/watch") return u.searchParams.get("v");
    const shorts = u.pathname.match(/^\/shorts\/([\w-]{6,})/);
    if (shorts) return shorts[1];
  } catch (_e) {
    /* fall through */
  }
  return null;
}

function currentVideo() {
  return (
    document.querySelector("video.html5-main-video") ||
    document.querySelector("video")
  );
}

function report(state) {
  const videoId = videoIdFromLocation();
  const video = currentVideo();
  if (!videoId || !video) return;
  const position = video.currentTime;
  if (!Number.isFinite(position)) return;
  try {
    chrome.runtime.sendMessage({
      kind: "media_state",
      url: location.href,
      video_id: videoId,
      position_s: Math.floor(position * 10) / 10,
      state,
    });
  } catch (_e) {
    // Extension reloaded/updated under us — context invalidated. Go quiet.
    stopTicking();
  }
}

function startTicking() {
  if (intervalId !== null) return;
  intervalId = setInterval(() => {
    const video = currentVideo();
    if (video && !video.paused && !video.ended) report("playing");
  }, REPORT_INTERVAL_MS);
}

function stopTicking() {
  if (intervalId !== null) {
    clearInterval(intervalId);
    intervalId = null;
  }
}

function attach() {
  const video = currentVideo();
  if (!video) return;
  if (video.dataset.apertureAttached === "1") return;
  video.dataset.apertureAttached = "1";
  video.addEventListener("pause", () => report("paused"));
  video.addEventListener("seeked", () => report("playing"));
  video.addEventListener("ended", () => report("paused"));
  video.addEventListener("play", () => {
    report("playing");
    startTicking();
  });
  if (!video.paused) {
    report("playing");
    startTicking();
  }
}

document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") report("paused");
});

// SPA navigation: re-resolve the video element and announce the new context.
window.addEventListener("yt-navigate-finish", () => {
  stopTicking();
  // The player node is often replaced across navigations; re-attach lazily.
  setTimeout(() => {
    attach();
    report("playing");
  }, 500);
});

attach();
// The player can mount after document_idle on cold loads.
const mountPoll = setInterval(() => {
  if (currentVideo()) {
    attach();
    clearInterval(mountPoll);
  }
}, 1000);
setTimeout(() => clearInterval(mountPoll), 30_000);
