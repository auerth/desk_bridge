//! Builds the [`AppState`] snapshot the UI renders.

use super::Engine;
use crate::ipc::{
    AppState, AudioFeatureDto, ConnState, ConnectionDto, DeviceDto, DiscoveredDto, FeatureDto,
    FeaturesDto, FocusDto, Live, MetricsDto, PairingDto, PeerDto, PlacementDto, ScreenDto,
    ScreensDto, UpdateDto,
};
use deskbridge_net::peers::Peer;
use deskbridge_net::short_fp;

fn peer_dto(p: &Peer, online: bool) -> PeerDto {
    PeerDto {
        id: p.id.to_string(),
        name: p.name.clone(),
        online,
        fingerprint_short: p.fp().map(|f| short_fp(&f)).unwrap_or_default(),
        last_ip: p.last_ip.clone(),
    }
}

impl Engine {
    pub(crate) fn build_state(&self) -> AppState {
        let own = self.own_id();
        let d = &self.cfg.device;
        let connected_peer = self.session.as_ref().map(|s| s.link.peer.id);

        let peers: Vec<PeerDto> = self
            .peers
            .all()
            .iter()
            .map(|p| {
                peer_dto(
                    p,
                    connected_peer == Some(p.id) || self.discovered.contains_key(&p.id),
                )
            })
            .collect();
        let discovered: Vec<DiscoveredDto> = self
            .discovered
            .values()
            .map(|e| DiscoveredDto {
                id: e.d.device_id.to_string(),
                name: e.d.name.clone(),
                address: e
                    .d
                    .socket_addrs()
                    .into_iter()
                    .find(std::net::SocketAddr::is_ipv4)
                    .or_else(|| e.d.socket_addrs().into_iter().next())
                    .map(|a| a.to_string())
                    .unwrap_or_default(),
                version: e.d.version.clone(),
                paired: self.peers.by_id(e.d.device_id).is_some(),
            })
            .collect();

        let connection = ConnectionDto {
            state: self.conn_state,
            peer: self.session.as_ref().map(|s| peer_dto(&s.link.peer, true)),
            rtt_us: self
                .clock
                .rtt_us()
                .map(|v| v.min(u64::from(u32::MAX)) as u32)
                .filter(|_| self.session.is_some()),
            clock_offset_us: self.clock.offset_us().filter(|_| self.session.is_some()),
            link: "unknown".into(),
            since_unix_ms: self.session.as_ref().map(|s| s.since_unix_ms),
            error: if self.session.is_none() {
                self.last_error.clone()
            } else {
                None
            },
            version_mismatch: self.version_mismatch,
            peer_version: self
                .session
                .as_ref()
                .map(|s| s.link.hello.app_version.clone()),
        };

        let connected = matches!(self.conn_state, ConnState::Connected | ConnState::Degraded);
        let input_state = if !self.cfg.input.enabled {
            Live::Off
        } else if connected
            && (self.router.is_remote() || self.receiver.as_ref().is_some_and(|r| r.is_active()))
        {
            Live::Live
        } else {
            Live::Ready
        };
        let mut features = FeaturesDto {
            input: FeatureDto {
                enabled: self.cfg.input.enabled,
                state: input_state,
                detail: None,
            },
            laptop_audio: AudioFeatureDto::default(),
            mic: AudioFeatureDto::default(),
            clipboard: FeatureDto {
                enabled: self.cfg.clipboard.send || self.cfg.clipboard.receive,
                state: if self.cfg.clipboard.send || self.cfg.clipboard.receive {
                    Live::Ready
                } else {
                    Live::Off
                },
                detail: None,
            },
            focus: FocusDto::default(),
        };
        self.fill_feature_state(&mut features);

        // screens of both devices
        let mut screens = Vec::new();
        for s in &self.local_screens {
            screens.push(ScreenDto {
                device_id: own.to_string(),
                id: s.id,
                x: s.x,
                y: s.y,
                w: s.w,
                h: s.h,
                dpi: s.dpi,
                primary: s.primary,
                name: s.name.clone(),
            });
        }
        if let Some(s) = &self.session {
            for r in &s.remote_screens {
                screens.push(ScreenDto {
                    device_id: s.link.peer.id.to_string(),
                    id: r.id,
                    x: r.x,
                    y: r.y,
                    w: r.w,
                    h: r.h,
                    dpi: r.dpi,
                    primary: r.primary,
                    name: r.name.clone(),
                });
            }
        }
        let placements = self
            .layout
            .to_placements()
            .into_iter()
            .map(|p| PlacementDto {
                device_id: p.device_id,
                screen_id: p.screen_id,
                x: p.x,
                y: p.y,
            })
            .collect();

        let location = if d.input_source {
            if self.router.is_remote() {
                "remote"
            } else {
                "local"
            }
        } else if self.receiver.as_ref().is_some_and(|r| r.is_active()) {
            "local"
        } else {
            "remote"
        };

        AppState {
            device: DeviceDto {
                id: own.to_string(),
                name: d.name.clone(),
                version: crate::brand::VERSION.into(),
                os: self.platform.os.clone(),
                input_source: d.input_source,
                headset_sink: d.headset_sink,
                stream_source: d.stream_source,
                onboarding_done: d.onboarding_done,
            },
            connection,
            peers,
            discovered,
            pairing: self.pairing.as_ref().map(|w| PairingDto {
                code: w.code().to_string(),
                seconds_left: w.seconds_left().min(u64::from(u32::MAX)) as u32,
            }),
            features,
            screens: ScreensDto {
                screens,
                placements,
                revision: self.layout.revision,
            },
            location: location.into(),
            cursor_locked: self.cursor_locked,
            warnings: self.warnings.clone(),
            transfers: self.transfers_snapshot(),
            update: UpdateDto {
                current: crate::brand::VERSION.into(),
                available: None,
                checking: false,
            },
            metrics: MetricsDto {
                rtt_history_us: self.rtt_history.iter().copied().collect(),
                input_latency_p50_us: self.input_latency_percentiles().0,
                input_latency_p95_us: self.input_latency_percentiles().1,
                input_latency_max_us: self.input_latency_percentiles().2,
                agent_cpu_pct: Some(self.proc_cpu),
                agent_rss_mb: Some(self.proc_rss),
                audio_buffer_fill_ms: self.audio_metrics().0,
                audio_drift_ppm: self.audio_metrics().1,
                audio_loss_pct: self.audio_metrics().2,
            },
            paused: self.cfg.general.paused,
        }
    }

    /// Feature modules overwrite their parts of the snapshot here.
    fn fill_feature_state(&self, f: &mut FeaturesDto) {
        self.fill_audio_state(f);
        self.fill_focus_state(f);
    }

    fn transfers_snapshot(&self) -> Vec<crate::ipc::TransferDto> {
        self.transfers_dto()
    }
}
