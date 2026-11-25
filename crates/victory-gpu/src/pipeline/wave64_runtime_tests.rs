//! Runtime compute dispatch and readback, using only the
//! synthetic containers from victory-shader's wave64_auto_group tests.

use super::*;
use std::time::{Duration, Instant};
use victory_shader::{Container, FourCC};
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

#[test]
#[ignore = "requires pre-generated synthetic wave64 containers, a wave32 GPU and D3D12 Graphics Tools"]
fn wave64_runtime_selects_paired_compute_pipelines() {
    let fixtures = std::path::PathBuf::from(
        std::env::var_os("VICTORY_TEST_WAVE64_DIR").expect("synthetic fixture directory"),
    );
    let device = &crate::host::host().expect("runtime D3D12 host").device;
    let mut caps = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
    // SAFETY: The feature query receives the matching description.
    unsafe {
        device
            .CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS1,
                (&raw mut caps).cast(),
                size_of_val(&caps) as u32,
            )
            .expect("wave capabilities");
    }
    assert_eq!((caps.WaveLaneCountMin, caps.WaveLaneCountMax), (32, 32));
    let messages: ID3D12InfoQueue = device.cast().expect("run with D3D12 debug enabled");
    // SAFETY: This isolated test owns the queue's observation window.
    unsafe { messages.ClearStoredMessages() };
    for threads in [64, 128] {
        let bytes = std::fs::read(fixtures.join(format!("wave64-{threads}.cdxil")))
            .expect("read the generated synthetic container");
        let desc = ComputeHead {
            root: std::ptr::null(),
            stage: Bytecode {
                code: bytes.as_ptr(),
                size: bytes.len(),
            },
        };
        let mut guest = std::ptr::null();
        assert_eq!(
            create_compute(std::ptr::null(), &desc, &PIPELINE_IIDS[0], &mut guest),
            object::S_OK,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(made) = host_pipeline(guest) {
                assert!(made.compute);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "runtime PSO for {threads} guest threads"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let translated = pipeline(guest).shaders[0]
            .translated()
            .expect("runtime translation");
        let dxil = Container::parse(&translated.host.dxil).expect("emitted DXIL");
        let psv = dxil.part(FourCC::PSV0).expect("pipeline state validation");
        // Four-byte size followed by the 36-byte prefix of PSVRuntimeInfo2.
        let physical_x = u32::from_le_bytes(psv.data[40..44].try_into().unwrap());
        assert_eq!(
            physical_x,
            threads / 2,
            "runtime did not select the paired form"
        );
        eprintln!("runtime PSO: {threads} guest threads, {physical_x} physical threads");
        let expected = std::fs::read(fixtures.join(format!("wave64-{threads}.expected")))
            .expect("read synthetic CPU reference");
        let expected: Vec<u32> = expected
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .map(u32::from_le_bytes)
            .collect();
        crate::draw::tests::dispatch_wave64_and_read_back(guest, threads, &expected);
        let release = object::slot::<extern "system" fn(This) -> u32>(guest, 0x10);
        release(guest);
    }
    // SAFETY: The queue supplies each message's required size. usize storage
    // provides enough alignment for the D3D12_MESSAGE header.
    unsafe {
        for index in 0..messages.GetNumStoredMessages() {
            let mut size = 0;
            messages.GetMessage(index, None, &mut size).unwrap();
            let mut store = vec![0usize; size.div_ceil(size_of::<usize>())];
            let message = store.as_mut_ptr().cast::<D3D12_MESSAGE>();
            messages
                .GetMessage(index, Some(message), &mut size)
                .unwrap();
            // A cold pipeline library can miss before the runtime builds
            // the PSO. Both successful PSOs were required above.
            if (*message).ID == D3D12_MESSAGE_ID_LOADPIPELINE_NAMENOTFOUND {
                continue;
            }
            assert!(
                (*message).Severity.0 > D3D12_MESSAGE_SEVERITY_WARNING.0,
                "{:?}: {}",
                (*message).ID,
                std::ffi::CStr::from_ptr((*message).pDescription.cast()).to_string_lossy(),
            );
        }
    }
}
