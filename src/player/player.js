// YouTube IFrame Player + WebSocket client for StreamLoop
(function () {
  'use strict';

  let ws = null;
  let player = null;
  let playerReady = false;
  let heartbeatTimer = null;
  let pendingSeekTime = 0;
  let currentLoop = false;
  let playlistLoadNotified = false;
  let currentQuality = '';
  const HEARTBEAT_INTERVAL = 5000;
  const WS_RECONNECT_DELAY = 3000;

  // --- WebSocket ---

  function connectWebSocket() {
    const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = protocol + '//' + location.host + '/ws';
    ws = new WebSocket(url);

    ws.onopen = function () {
      console.log('[WS] Connected');
      ws.send(JSON.stringify({ type: 'ready' }));
      startHeartbeat();
    };

    ws.onmessage = function (event) {
      var msg;
      try {
        msg = JSON.parse(event.data);
      } catch (e) {
        console.error('[WS] Bad message:', e);
        return;
      }
      handleServerMessage(msg);
    };

    ws.onclose = function () {
      console.warn('[WS] Disconnected, reconnecting...');
      stopHeartbeat();
      setTimeout(connectWebSocket, WS_RECONNECT_DELAY);
    };

    ws.onerror = function (err) {
      console.error('[WS] Error:', err);
    };
  }

  function sendMessage(msg) {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify(msg));
    }
  }

  function getNextVideoId() {
    if (!player || typeof player.getPlaylist !== 'function') return '';
    try {
      var playlist = player.getPlaylist();
      if (!playlist || playlist.length === 0) return '';
      var currentIndex = player.getPlaylistIndex() || 0;
      var nextIndex = currentIndex + 1;
      return nextIndex < playlist.length ? playlist[nextIndex] : '';
    } catch (e) {
      return '';
    }
  }

  function startHeartbeat() {
    stopHeartbeat();
    heartbeatTimer = setInterval(function () {
      if (!player || typeof player.getPlayerState !== 'function') return;
      sendMessage({
        type: 'heartbeat',
        videoIndex: player.getPlaylistIndex() || 0,
        videoId: getVideoId(),
        videoTitle: getVideoTitle(),
        playerState: player.getPlayerState() ?? -1,
        currentTime: player.getCurrentTime() || 0,
        videoDuration: player.getDuration() || 0,
        nextVideoId: getNextVideoId(),
        volume: player.getVolume(),
        muted: player.isMuted(),
        playbackQuality: currentQuality,
      });
      updateOverlay();
    }, HEARTBEAT_INTERVAL);
  }

  function stopHeartbeat() {
    if (heartbeatTimer) {
      clearInterval(heartbeatTimer);
      heartbeatTimer = null;
    }
  }

  function getVideoId() {
    if (!player || typeof player.getVideoUrl !== 'function') return '';
    try {
      var url = player.getVideoUrl();
      var match = url && url.match(/[?&]v=([^&]+)/);
      return match ? match[1] : '';
    } catch (e) {
      return '';
    }
  }

  function getVideoTitle() {
    if (!player || typeof player.getVideoData !== 'function') return '';
    try {
      var data = player.getVideoData();
      return (data && data.title) || '';
    } catch (e) {
      return '';
    }
  }

  // --- Server message handler ---

  function handleServerMessage(msg) {
    console.log('[WS] Server:', msg.type, msg);
    switch (msg.type) {
      case 'loadPlaylist':
        loadPlaylist(msg.playlistId, msg.index, msg.loop, msg.startTime);
        break;
      case 'retryCurrent':
        retryCurrent();
        break;
      case 'resume':
        resumePlayback();
        break;
      case 'skip':
        skipTo(msg.index);
        break;
    }
  }

  function loadPlaylist(playlistId, index, loop, startTime) {
    if (!playerReady) {
      console.warn('[Player] Not ready yet, deferring loadPlaylist');
      setTimeout(function () { loadPlaylist(playlistId, index, loop, startTime); }, 500);
      return;
    }
    // Stop current playback immediately to prevent YouTube from auto-advancing
    // the old playlist while we load the new one
    player.stopVideo();
    pendingSeekTime = startTime || 0;
    playlistLoadNotified = false;
    player.loadPlaylist({
      list: playlistId,
      listType: 'playlist',
      index: index || 0,
    });
    currentLoop = !!loop;
    player.setLoop(currentLoop);
  }

  function retryCurrent() {
    if (!player) return;
    var index = player.getPlaylistIndex();
    player.playVideoAt(index);
  }

  function resumePlayback() {
    if (!player) return;
    player.playVideo();
  }

  function skipTo(index) {
    if (!player) return;
    player.playVideoAt(index);
  }

  // --- YouTube IFrame API ---

  window.onYouTubeIframeAPIReady = function () {
    player = new YT.Player('player', {
      width: '100%',
      height: '100%',
      playerVars: {
        autoplay: 1,
        controls: 0,
        disablekb: 1,
        fs: 0,
        rel: 0,
        iv_load_policy: 3,
        cc_load_policy: 0,
      },
      events: {
        onReady: onPlayerReady,
        onStateChange: onPlayerStateChange,
        onError: onPlayerError,
        onPlaybackQualityChange: onPlaybackQualityChange,
      },
    });
  };

  function onPlayerReady() {
    console.log('[Player] Ready');
    playerReady = true;
    player.unMute();
    player.setVolume(100);
    // Server will send loadPlaylist after receiving 'ready' via WebSocket
  }

  function onPlayerStateChange(event) {
    var state = event.data;
    sendMessage({
      type: 'stateChange',
      playerState: state,
      videoIndex: player.getPlaylistIndex() || 0,
      videoId: getVideoId(),
      videoTitle: getVideoTitle(),
    });
    updateOverlay();

    // When last video ends in a non-loop playlist, stop playback immediately
    // to prevent YouTube from auto-advancing back to video 0 before the server
    // can send the loadPlaylist command for the next playlist
    if (state === YT.PlayerState.ENDED && !currentLoop) {
      try {
        var playlist = player.getPlaylist();
        var idx = player.getPlaylistIndex();
        if (playlist && playlist.length > 0 && idx === playlist.length - 1) {
          console.log('[Player] Last video ended (non-loop), stopping to wait for server');
          player.stopVideo();
        }
      } catch (e) { /* ignore */ }
    }

    // Ensure audio is never muted (autoplay policy can mute videos)
    if (state === YT.PlayerState.PLAYING) {
      player.unMute();
      player.setVolume(100);
    }

    // Seek to saved position after resume from crash
    if (state === YT.PlayerState.PLAYING && pendingSeekTime > 0) {
      var seekTo = pendingSeekTime;
      pendingSeekTime = 0;
      console.log('[Player] Seeking to saved position:', seekTo);
      player.seekTo(seekTo, true);
    }

    // Notify server when playlist is loaded (first CUED or PLAYING after load)
    if (!playlistLoadNotified && (state === YT.PlayerState.CUED || state === YT.PlayerState.PLAYING)) {
      try {
        var playlist = player.getPlaylist();
        if (playlist && playlist.length > 0) {
          playlistLoadNotified = true;
          sendMessage({
            type: 'playlistLoaded',
            totalVideos: playlist.length,
          });
        }
      } catch (e) { /* ignore */ }
    }
  }

  function onPlayerError(event) {
    console.error('[Player] Error:', event.data);
    sendMessage({
      type: 'error',
      errorCode: event.data,
      videoIndex: player.getPlaylistIndex() || 0,
      videoId: getVideoId(),
    });
  }

  function onPlaybackQualityChange(event) {
    currentQuality = event.data || '';
    console.log('[Player] Quality changed:', currentQuality);
  }

  // --- Now Playing overlay ---

  var npOverlay = document.getElementById('np-overlay');
  var npTitle = document.getElementById('np-title');
  var npLabel = document.getElementById('np-label');
  var lastOverlayTitle = '';

  function updateOverlay() {
    var title = getVideoTitle();
    if (title !== lastOverlayTitle) {
      lastOverlayTitle = title;
      if (npTitle) npTitle.textContent = title;
    }
    if (npOverlay) {
      if (title) {
        npOverlay.classList.remove('np-hidden');
      } else {
        npOverlay.classList.add('np-hidden');
      }
    }
    if (npLabel && player) {
      var duration = player.getDuration() || 0;
      var current = player.getCurrentTime() || 0;
      if (duration > 0) {
        var pct = Math.min(100, Math.round((current / duration) * 100));
        npLabel.textContent = 'Now Playing, progress: ' + pct + '%';
      } else {
        npLabel.textContent = 'Now Playing';
      }
    }
  }

  // --- Boot ---

  // Load YouTube IFrame API script
  var tag = document.createElement('script');
  tag.src = 'https://www.youtube.com/iframe_api';
  var firstScript = document.getElementsByTagName('script')[0];
  firstScript.parentNode.insertBefore(tag, firstScript);

  // Connect WebSocket
  connectWebSocket();
})();
