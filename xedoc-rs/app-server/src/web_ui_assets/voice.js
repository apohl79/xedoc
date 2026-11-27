"use strict";

(() => {
  const button = document.getElementById("voice");
  const optionsButton = document.getElementById("voice-options");
  const menu = document.getElementById("voice-menu");
  const backend = document.getElementById("voice-backend");
  const backendNote = document.querySelector(".voice-menu-note");
  const status = document.getElementById("voice-status");
  const sessionMatch = location.pathname.match(/^\/session-ui\/([^/]+)(?:\/|$)/);
  if (!button || !optionsButton || !menu || !backend || !backendNote || !status || !sessionMatch) return;

  const guiToken = sessionMatch[1];
  const AudioContext = window.AudioContext || window.webkitAudioContext;
  const targetSampleRate = 16_000;
  const silenceThreshold = 0.018;
  const silenceDurationMs = 800;
  const maxRecordingDurationMs = 24_000;
  const maxSynthesisTextBytes = 1000;
  const encoder = new TextEncoder();
  let activeSession = null;
  let voiceAvailable = false;

  button.hidden = false;
  optionsButton.hidden = false;
  status.hidden = false;
  button.disabled = true;

  function setStatus(message) {
    status.textContent = message;
  }

  function positionMenu(x, y, alignRight = false, above = false) {
    menu.hidden = false;
    const bounds = menu.getBoundingClientRect();
    const left = alignRight ? x - bounds.width : x;
    const top = above ? y - bounds.height : y;
    menu.style.left = `${Math.max(8, Math.min(left, innerWidth - bounds.width - 8))}px`;
    menu.style.top = `${Math.max(8, Math.min(top, innerHeight - bounds.height - 8))}px`;
    optionsButton.setAttribute("aria-expanded", "true");
  }

  function closeMenu() {
    menu.hidden = true;
    optionsButton.setAttribute("aria-expanded", "false");
  }

  function isActive(session) {
    return activeSession === session && !session.closed;
  }

  function canWork(session) {
    return isActive(session) && !session.closing;
  }

  function stopMicrophone(microphone) {
    for (const track of microphone?.getTracks() || []) track.stop();
  }

  async function post(path, body) {
    const response = await fetch(path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ guiToken, ...body }),
    });
    const result = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(result.error || "Voice request failed");
    return result;
  }

  async function loadVoiceStatus() {
    setStatus("Checking voice availability…");
    try {
      const result = await post("/api/session/voice/mode", {});
      voiceAvailable = result.available === true;
      if (voiceAvailable) {
        backend.textContent = "Local Whisper + Piper";
        backendNote.textContent = "Speech runs on the host; models download on first use.";
        setStatus("Local voice is ready.");
      } else {
        backendNote.textContent = "Configure OpenAI OAuth or an API key to use voice.";
        setStatus("Voice unavailable. Configure OpenAI OAuth or an API key.");
      }
    } catch {
      voiceAvailable = false;
      backendNote.textContent = "Voice availability could not be checked.";
      setStatus("Voice availability could not be checked.");
    }
    backend.setAttribute("aria-checked", voiceAvailable ? "true" : "false");
    button.disabled = !voiceAvailable;
    backend.disabled = !voiceAvailable;
    optionsButton.hidden = !voiceAvailable;
    button.title = voiceAvailable ? "Start voice conversation" : "Voice unavailable";
  }

  function downsampleToPcm16(samples, sourceRate) {
    const ratio = sourceRate / targetSampleRate;
    const output = new Int16Array(Math.floor(samples.length / ratio));
    for (let index = 0; index < output.length; index += 1) {
      const start = Math.floor(index * ratio);
      const end = Math.min(Math.floor((index + 1) * ratio), samples.length);
      let sum = 0;
      for (let sourceIndex = start; sourceIndex < end; sourceIndex += 1) {
        sum += samples[sourceIndex];
      }
      const value = sum / Math.max(1, end - start);
      output[index] = value < 0
        ? Math.round(Math.max(-1, value) * 32768)
        : Math.round(Math.min(1, value) * 32767);
    }
    return output;
  }

  function wavBase64(chunks) {
    const sampleCount = chunks.reduce((total, chunk) => total + chunk.length, 0);
    const bytes = new ArrayBuffer(44 + sampleCount * 2);
    const view = new DataView(bytes);
    const writeText = (offset, value) => {
      for (let index = 0; index < value.length; index += 1) {
        view.setUint8(offset + index, value.charCodeAt(index));
      }
    };
    writeText(0, "RIFF");
    view.setUint32(4, 36 + sampleCount * 2, true);
    writeText(8, "WAVE");
    writeText(12, "fmt ");
    view.setUint32(16, 16, true);
    view.setUint16(20, 1, true);
    view.setUint16(22, 1, true);
    view.setUint32(24, targetSampleRate, true);
    view.setUint32(28, targetSampleRate * 2, true);
    view.setUint16(32, 2, true);
    view.setUint16(34, 16, true);
    writeText(36, "data");
    view.setUint32(40, sampleCount * 2, true);
    let destination = 44;
    for (const chunk of chunks) {
      for (const sample of chunk) {
        view.setInt16(destination, sample, true);
        destination += 2;
      }
    }
    const binary = new Uint8Array(bytes);
    let encoded = "";
    const blockSize = 0x8000;
    for (let offset = 0; offset < binary.length; offset += blockSize) {
      encoded += String.fromCharCode(...binary.subarray(offset, offset + blockSize));
    }
    return btoa(encoded);
  }

  function decodeBase64(value) {
    const binary = atob(value);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) {
      bytes[index] = binary.charCodeAt(index);
    }
    return bytes.buffer;
  }

  function splitSynthesisText(text) {
    const chunks = [];
    let current = "";
    for (const character of Array.from(text)) {
      if (encoder.encode(current + character).length > maxSynthesisTextBytes && current) {
        const boundary = current.lastIndexOf(" ");
        if (boundary > 0) {
          chunks.push(current.slice(0, boundary + 1));
          current = current.slice(boundary + 1);
        } else {
          chunks.push(current);
          current = "";
        }
      }
      current += character;
    }
    if (current.trim()) chunks.push(current);
    return chunks;
  }

  function stopCapture(session) {
    if (session.processor) {
      session.processor.onaudioprocess = null;
      session.processor.disconnect();
      session.processor = null;
    }
    session.source?.disconnect();
    session.source = null;
    session.silentGain?.disconnect();
    session.silentGain = null;
    stopMicrophone(session.microphone);
    session.microphone = null;
  }

  function finishVoice(session, message = "Voice conversation stopped") {
    if (session.closed) return;
    session.closed = true;
    session.closing = false;
    stopCapture(session);
    try {
      session.playback?.stop();
    } catch {
      // Playback may already have ended.
    }
    session.playback = null;
    if (session.audioContext && session.audioContext.state !== "closed") {
      void session.audioContext.close();
    }
    if (activeSession !== session) return;
    activeSession = null;
    button.disabled = false;
    button.setAttribute("aria-pressed", "false");
    button.setAttribute("aria-label", "Start voice conversation");
    button.title = "Start voice conversation";
    setStatus(message);
  }

  function stopVoice(session, message = "Voice conversation stopped") {
    if (!isActive(session)) return;
    session.closing = true;
    finishVoice(session, message);
  }

  async function playAudio(session, audioBase64) {
    if (!canWork(session)) return;
    const buffer = await session.audioContext.decodeAudioData(decodeBase64(audioBase64));
    if (!canWork(session)) return;
    const source = session.audioContext.createBufferSource();
    session.playback = source;
    source.buffer = buffer;
    source.connect(session.audioContext.destination);
    await new Promise((resolve) => {
      source.addEventListener("ended", resolve, { once: true });
      source.start();
    });
    source.disconnect();
    if (session.playback === source) session.playback = null;
  }

  async function speakResponse(session, text) {
    const locale = navigator.language || "en-US";
    const chunks = splitSynthesisText(text);
    for (const chunk of chunks) {
      if (!canWork(session)) return;
      const result = await post("/api/session/voice/synthesize", { text: chunk, locale });
      if (!canWork(session)) return;
      await playAudio(session, result.audioBase64);
    }
  }

  async function processUtterance(session) {
    const samples = session.samples;
    session.samples = [];
    session.preRoll = [];
    session.speechStarted = false;
    session.silenceMs = 0;
    session.recordingMs = 0;
    session.busy = true;
    try {
      if (!samples.length) return;
      setStatus("Transcribing locally on the host…");
      const locale = navigator.language || "en-US";
      const transcript = await post("/api/session/voice/transcribe", {
        audioBase64: wavBase64(samples),
        locale,
      });
      if (!canWork(session)) return;
      if (typeof transcript.text !== "string" || !transcript.text.trim()) {
        throw new Error("No speech was recognized. Try again.");
      }
      setStatus("Xedoc is responding…");
      const result = await post("/api/session/voice/turn", { transcript: transcript.text });
      if (!canWork(session)) return;
      setStatus("Speaking Xedoc’s response…");
      await speakResponse(session, result.text);
      if (canWork(session)) setStatus("Listening…");
    } catch (error) {
      if (!canWork(session)) return;
      setStatus(error instanceof Error ? error.message : "Voice request failed.");
    } finally {
      session.busy = false;
      if (canWork(session)) void startCapture(session);
    }
  }

  function handleAudioFrame(session, samples) {
    if (!canWork(session) || session.busy || !session.processor) return;
    let energy = 0;
    for (const sample of samples) energy += sample * sample;
    const loudness = Math.sqrt(energy / samples.length);
    const durationMs = (samples.length / session.audioContext.sampleRate) * 1000;
    const pcm = downsampleToPcm16(samples, session.audioContext.sampleRate);

    if (!session.speechStarted) {
      session.preRoll.push(pcm);
      session.preRollMs += durationMs;
      while (session.preRollMs > 250 && session.preRoll.length > 1) {
        const removed = session.preRoll.shift();
        session.preRollMs -= (removed.length / targetSampleRate) * 1000;
      }
      if (loudness < silenceThreshold) return;
      session.speechStarted = true;
      session.samples = session.preRoll;
      session.recordingMs = session.preRollMs;
      session.preRoll = [];
      session.preRollMs = 0;
    } else {
      session.samples.push(pcm);
    }
    session.recordingMs += durationMs;

    if (loudness >= silenceThreshold) {
      session.silenceMs = 0;
    } else {
      session.silenceMs += durationMs;
    }

    if (session.recordingMs >= maxRecordingDurationMs || session.silenceMs >= silenceDurationMs) {
      stopCapture(session);
      void processUtterance(session);
    }
  }

  async function startCapture(session, initialStream = null) {
    if (!canWork(session) || session.busy || session.captureStarting || session.microphone) {
      stopMicrophone(initialStream);
      return;
    }
    session.captureStarting = true;
    try {
      const microphone = initialStream || await navigator.mediaDevices.getUserMedia({
        audio: {
          channelCount: 1,
          echoCancellation: true,
          noiseSuppression: true,
          autoGainControl: true,
        },
      });
      if (!canWork(session)) {
        for (const track of microphone.getTracks()) track.stop();
        return;
      }
      session.microphone = microphone;
      session.source = session.audioContext.createMediaStreamSource(microphone);
      session.processor = session.audioContext.createScriptProcessor(4096, 1, 1);
      session.silentGain = session.audioContext.createGain();
      session.silentGain.gain.value = 0;
      session.processor.onaudioprocess = (event) => {
        handleAudioFrame(session, event.inputBuffer.getChannelData(0));
      };
      session.source.connect(session.processor);
      session.processor.connect(session.silentGain);
      session.silentGain.connect(session.audioContext.destination);
      setStatus("Listening…");
      button.disabled = false;
    } catch {
      if (canWork(session)) finishVoice(session, "Allow microphone access to use voice.");
    } finally {
      session.captureStarting = false;
    }
  }

  async function startVoice() {
    if (!AudioContext || !navigator.mediaDevices?.getUserMedia) {
      setStatus("Audio capture is unavailable in this browser.");
      return;
    }
    const session = {
      audioContext: null,
      microphone: null,
      source: null,
      processor: null,
      silentGain: null,
      playback: null,
      samples: [],
      preRoll: [],
      preRollMs: 0,
      recordingMs: 0,
      silenceMs: 0,
      speechStarted: false,
      busy: false,
      captureStarting: false,
      closed: false,
      closing: false,
    };
    activeSession = session;
    button.disabled = true;
    button.setAttribute("aria-pressed", "true");
    button.setAttribute("aria-label", "Stop voice conversation");
    button.title = "Stop voice conversation";
    setStatus("Requesting microphone…");
    let microphoneRequest;
    try {
      microphoneRequest = navigator.mediaDevices.getUserMedia({
        audio: {
          channelCount: 1,
          echoCancellation: true,
          noiseSuppression: true,
          autoGainControl: true,
        },
      });
      session.audioContext = new AudioContext();
      await Promise.all([microphoneRequest, session.audioContext.resume()]);
      const microphone = await microphoneRequest;
      await startCapture(session, microphone);
    } catch {
      if (microphoneRequest) {
        void microphoneRequest.then(stopMicrophone, () => {});
      }
      if (isActive(session)) finishVoice(session, "Allow microphone access to use voice.");
    }
  }

  button.addEventListener("click", () => {
    if (activeSession) stopVoice(activeSession);
    else if (voiceAvailable) void startVoice();
  });
  optionsButton.addEventListener("click", () => {
    if (menu.hidden) {
      const bounds = optionsButton.getBoundingClientRect();
      positionMenu(bounds.right, bounds.top - 8, true, true);
    } else {
      closeMenu();
    }
  });
  optionsButton.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    positionMenu(event.clientX, event.clientY);
  });
  button.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    positionMenu(event.clientX, event.clientY);
  });
  backend.addEventListener("click", () => {
    if (!voiceAvailable) return;
    backend.setAttribute("aria-checked", "true");
    setStatus("Local speech is selected; this session’s Xedoc model handles reasoning.");
    closeMenu();
  });
  document.addEventListener("pointerdown", (event) => {
    if (!menu.hidden && !menu.contains(event.target)
        && event.target !== optionsButton && event.target !== button) closeMenu();
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") closeMenu();
  });
  window.addEventListener("message", (event) => {
    if (event.source !== window.parent || event.origin !== location.origin
        || event.data?.type !== "xedoc-session-hidden") return;
    if (activeSession) stopVoice(activeSession, "Voice stopped when this session was hidden.");
  });
  window.addEventListener("pagehide", () => {
    if (activeSession) finishVoice(activeSession, "");
  });
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden" && activeSession) stopVoice(activeSession);
  });
  void loadVoiceStatus();
})();
