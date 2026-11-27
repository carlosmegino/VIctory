// Adapted from Dathinel/victory-4gb commit 6622a0cd30e5669206dbda29bb7670e5ee8e20d0.
// Guest instructions and CPU oracle are unchanged. The metadata expectation
// identifies the paired layout; route diagnostics are recorded separately.
//! Synthetic two-wave contract for Marvisak/VIctory#2688. The LDS loop
//! is adapted from PiloteCode's GPL-3.0 fixture in fxceless/victory-fork#854.

use super::host::{Dispatch, Host, Root, View};
use super::validator;
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{Container, FourCC, dxbc::u32_at};
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

const EXEC: u32 = 126;
const VCC: u32 = 106;
const GUARD: usize = 8;
const OUTPUTS: usize = 7;

const fn k(value: u32) -> u32 {
    128 + value
}
const fn v(reg: u32) -> u32 {
    256 + reg
}
const fn mov(dst: u32, src: u32) -> u32 {
    0x7E00_0200 | dst << 17 | src
}
const fn sop1(op: u32, dst: u32, src: u32) -> u32 {
    0xBE80_0000 | dst << 16 | op << 8 | src
}
const fn alu(op: u32, dst: u32, left: u32, right: u32) -> u32 {
    op << 25 | dst << 17 | right << 9 | left
}
const fn cmp(op: u32, left: u32, right: u32) -> u32 {
    0x7C00_0000 | op << 17 | right << 9 | left
}
const fn ds(op: u32, dst: u32, address: u32, data: u32) -> [u32; 2] {
    [0xD800_0000 | op << 18, dst << 24 | data << 8 | address]
}

fn guest(threads: u32) -> NativeProgram {
    let mut words = vec![
        alu(0x16, 1, k(6), 0),  // v1: logical wave index
        alu(0x1B, 2, k(63), 0), // v2: logical lane index
        alu(0x1A, 11, k(2), 1), // v11: byte address of wave's LDS counter
        mov(12, k(0)),
        sop1(4, 22, EXEC),
        cmp(0xD2, k(0), 2), // only logical lane 0 initializes this wave's counter
    ];
    words.extend(ds(0x0D, 0, 11, 12));
    words.extend([
        sop1(4, EXEC, 22),
        alu(0x1B, 3, k(3), 2),
        alu(0x25, 3, k(1), 3),
        alu(0x25, 3, v(1), 3), // rounds = 1 + (lane & 3) + wave
        mov(4, k(0)),
        mov(13, k(1)),
        sop1(4, 20, EXEC),
        alu(0x25, 4, v(2), 4), // loop: accumulate the lane number
    ]);
    words.extend(ds(0, 0, 11, 13)); // ds_add_u32: add one per participating lane
    words.extend([
        alu(0x25, 3, 193, 3), // decrement remaining rounds
        cmp(0xC5, k(0), 3),
        0x8000_0000 | 0x0F << 23 | EXEC << 16 | VCC << 8 | EXEC, // s_and_b64 exec, exec, vcc
        0xBF89_FFF9,                                             // s_cbranch_execnz loop
        sop1(4, EXEC, 20),
    ]);
    words.extend(ds(0x36, 5, 11, 0)); // v5: total from own wave

    // Wave 0 first reads lane 40 and then lane 20. Wave 1 selects lane 24
    // and then lane 36, so the same instruction selects opposite halves.
    // Alternate the elected half by wave parity for groups with four waves.
    let selector = if threads == 256 {
        words.push(alu(0x1B, 8, k(1), 1)); // v8 = wave index & 1
        8
    } else {
        1
    };
    for (threshold, op, scalar, dest) in [(39, 0x26, 6, 6), (19, 0x25, 7, 9)] {
        words.extend([
            alu(0x1A, 14, k(4), selector),
            alu(op, 14, k(threshold), 14),
            sop1(4, 24, EXEC),
            cmp(0xD1, v(14), 2),               // v_cmpx_lt_u32 threshold, lane
            0x7E00_0400 | scalar << 17 | v(4), // v_readfirstlane_b32
            sop1(4, EXEC, 24),
            mov(dest, scalar),
        ]);
    }

    // Write one value per lane into its own wave's region, then read its
    // opposite half. The guest needs ordering within its wave without s_barrier.
    words.extend([
        alu(0x1A, 10, k(2), 0),
        alu(0x1D, 14, k(32), 0),
        alu(0x1A, 14, k(2), 14),
    ]);
    let counter_bytes = (threads / 64 * 4).max(8);
    let [op, args] = ds(0x0D, 0, 10, 4);
    words.extend([op | counter_bytes, args]);
    let [op, args] = ds(0x36, 10, 14, 0);
    words.extend([op | counter_bytes, args]);

    if threads >= 128 {
        words.extend([0xBF8A_0000, alu(0x1D, 14, k(4), 11)]); // s_barrier, then other wave's LDS
        words.extend(ds(0x36, 7, 14, 0));
    } else {
        words.push(mov(7, v(5)));
    }
    words.extend([alu(0x1A, 15, k(2), 0), 0xBE85_0380]); // output offset and table address high
    for (slot, reg) in [4, 5, 6, 9, 7, 10, 0].into_iter().enumerate() {
        words.extend([
            0x3D << 26 | 0x02 << 18 | 8 << 6 | 2, // s_load_dwordx4 s[8:11], s[4:5]
            125 << 25 | (slot as u32 * 32),
            0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
            0xE070_1000,
            0x8002_0000 | reg << 8 | 15, // buffer_store_dword v<reg>, v15, s[8:11], 0 offen
        ]);
    }
    words.push(0xBF81_0000);
    NativeProgram {
        stage: HwStage::Cs,
        wave64: true,
        vgprs: 16,
        layout: vec![RootParam {
            root: RootType::Table,
            table_offset_dwords: 0,
            sgprs: 1,
            base_register: 0,
            user_data: 4,
            slot: 0,
        }],
        used: (0..OUTPUTS as u32)
            .map(|slot| UsedResource {
                kind: ResourceType::Uav,
                root: RootType::Table,
                table_offset_dwords: slot * 8,
                lower_bound: slot,
                stride: 0,
                dwords: 4,
                user_data: 4,
                slot: 0,
            })
            .collect(),
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1 | ((counter_bytes + threads * 4).div_ceil(512).max(2) << 15),
            threads: [threads, 1, 1],
        }),
        code: words.into_iter().flat_map(u32::to_le_bytes).collect(),
    }
}

fn oracle(threads: u32, thread: u32) -> [u32; OUTPUTS] {
    let wave = thread / 64;
    let lane = thread % 64;
    let lane_sum = |lane| lane * (1 + (lane % 4) + wave);
    let wave_total = |wave| (0..64).map(|lane| 1 + lane % 4 + wave).sum();
    [
        lane_sum(lane),
        wave_total(wave),
        lane_sum([40, 24][(wave & 1) as usize]),
        lane_sum([20, 36][(wave & 1) as usize]),
        wave_total(if threads >= 128 { wave ^ 1 } else { wave }),
        lane_sum(lane ^ 32),
        thread,
    ]
}

fn debug_warnings(queue: &ID3D12InfoQueue) -> Vec<String> {
    // SAFETY: All read buffers are aligned and sized by the info queue.
    unsafe {
        (0..queue.GetNumStoredMessages())
            .filter_map(|index| {
                let mut size = 0;
                queue.GetMessage(index, None, &mut size).unwrap();
                let mut words = vec![0usize; size.div_ceil(size_of::<usize>())];
                let message = words.as_mut_ptr().cast::<D3D12_MESSAGE>();
                queue.GetMessage(index, Some(message), &mut size).unwrap();
                let message = &*message;
                (message.Severity.0 <= D3D12_MESSAGE_SEVERITY_WARNING.0).then(|| {
                    std::ffi::CStr::from_ptr(message.pDescription.cast())
                        .to_string_lossy()
                        .into_owned()
                })
            })
            .collect()
    }
}

fn run(threads: u32) {
    static DEBUG: std::sync::Once = std::sync::Once::new();
    // SAFETY: These opt-in tests run in isolation and enable the debug layer
    // before creating the first device in the test process.
    DEBUG.call_once(|| unsafe {
        let mut debug: Option<ID3D12Debug> = None;
        D3D12GetDebugInterface(&mut debug).expect("D3D12 Graphics Tools");
        let debug = debug.expect("D3D12 debug interface");
        debug.EnableDebugLayer();
        debug
            .cast::<ID3D12Debug1>()
            .unwrap()
            .SetEnableGPUBasedValidation(true);
    });
    let host = Host::new(false).expect("hardware D3D12 adapter");
    // SAFETY: The feature query writes only into the matching D3D12 description.
    let mut caps = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
    unsafe {
        host.device
            .CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS1,
                (&raw mut caps).cast(),
                size_of_val(&caps) as u32,
            )
            .expect("wave capabilities");
    }
    assert_eq!((caps.WaveLaneCountMin, caps.WaveLaneCountMax), (32, 32));
    let queue: ID3D12InfoQueue = host.device.cast().expect("D3D12 info queue");
    // SAFETY: The queue belongs to this test's device.
    unsafe { queue.ClearStoredMessages() };
    // Use the same host capability as the runtime and enter through the
    // public translator. Neither the paired form nor a wave override is used.
    super::set_host_max_wave(caps.WaveLaneCountMax);
    let bytes = crate::native::tests::compute_uav_container(&guest(threads));
    let mut shader = super::translate(&bytes).expect("translate synthetic wave64 container");
    validator::Validator::load(validator::Validator::default_path().as_deref())
        .expect("DXIL validator")
        .validate(&mut shader.dxil)
        .expect("validate synthetic DXIL");
    let (root, pso) = host.compute_pipeline(&shader).unwrap_or_else(|error| {
        panic!(
            "compute PSO: {error}; D3D12 validation: {:#?}",
            debug_warnings(&queue)
        );
    });
    let emitted = Container::parse(&shader.dxil).unwrap();
    let psv = emitted.part(FourCC::PSV0).unwrap();
    eprintln!(
        "ROUTE: guest={}, physical={}, wave-range={}..{}",
        threads,
        u32_at(psv.data, 40).unwrap(),
        u32_at(psv.data, 20).unwrap(),
        u32_at(psv.data, 24).unwrap()
    );
    let views = [(); OUTPUTS].map(|()| View::RawUav(threads + GUARD as u32));
    let data = vec![0; shader.user_data_dwords().max(1) as usize];
    let out = host
        .dispatch(
            &root,
            &pso,
            &Dispatch {
                roots: vec![(0, Root::Constants(data))],
                views: &views,
                samplers: &[],
                groups: [1, 1, 1],
            },
        )
        .expect("dispatch and readback");
    let warnings = debug_warnings(&queue);
    assert!(warnings.is_empty(), "D3D12 validation: {warnings:#?}");
    assert_eq!(out.len(), OUTPUTS);
    for (slot, words) in out.iter().enumerate() {
        assert_eq!(words.len(), threads as usize + GUARD);
        for thread in 0..threads {
            assert_eq!(
                words[thread as usize],
                oracle(threads, thread)[slot],
                "UAV {slot}, thread {thread}"
            );
        }
        assert_eq!(&words[threads as usize..], &[0; GUARD], "UAV {slot} guard");
    }
    let container = Container::parse(&shader.dxil).expect("emitted DXIL container");
    let psv = container
        .part(FourCC::PSV0)
        .expect("pipeline state validation");
    assert_eq!(
        u32_at(psv.data, 40).unwrap(),
        threads.div_ceil(64) * 32,
        "paired metadata: one physical Wave32 per logical Wave64, including a partial wave"
    );
    eprintln!(
        "{threads} guest threads: {OUTPUTS} UAVs match CPU, guards intact, no D3D12 warnings/errors"
    );
}

// Select only these tests so GPU validation is enabled before device creation.
#[test]
#[ignore = "requires a wave32 GPU, Graphics Tools, and DXIL validator"]
fn wave64_auto_group_64() {
    run(64);
}

#[test]
#[ignore = "requires a wave32 GPU, Graphics Tools, and DXIL validator"]
fn wave64_auto_group_128() {
    run(128);
}

#[test]
#[ignore = "requires a wave32 GPU, Graphics Tools, and DXIL validator"]
fn wave64_auto_group_256() {
    run(256);
}
