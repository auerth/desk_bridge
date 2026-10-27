//! Video backends for focus mode: DXGI Desktop Duplication capture, GPU colour conversion and the
//! native Direct3D 11 presenter window. Encoding/decoding lives in `deskbridge-video` (FFmpeg).

pub mod capture;
pub mod common;
pub mod present;
pub mod secure;
pub mod vp;

pub use capture::DxgiCapturer;
pub use common::{
    D3dDevice, GpuFrame, OutputInfo, adapter_by_index, adapter_by_luid, create_device,
    device_adapter_info, enable_multithread_protection, find_output_by_rect, list_outputs, now_us,
    qpc_to_us,
};
pub use present::{D3dPresenter, PresentMode, PresenterEvent, PxRect};
pub use secure::{
    CopyOutcome, Pointer, ProducerInfo, Pump, SecureConsumer, SecureFrameSource, SecurePoll,
    SecureProducer, SecureStartInfo, SecureUpdate,
};
pub use vp::{ConvKind, VpConverter};
