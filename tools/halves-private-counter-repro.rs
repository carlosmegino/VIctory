//! Synthetic #2633 compatibility diagnostic; Dathinel 561503d guest/oracle and PiloteCode harness attribution retained.
//! Adapted from the tested NEW-354 Windows/Blackwell diagnostic. No title inputs.
use super::host::{Dispatch, Host, Root, View};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;
fn checked_host() -> Host {
    static DEBUG: std::sync::Once = std::sync::Once::new();
    let debug = std::env::var("VICTORY_REPRO_NO_DEBUG").as_deref() != Ok("1");
    DEBUG.call_once(|| {
        if debug {
            // SAFETY: selected diagnostic tests run alone, before device creation.
            unsafe {
                let mut layer: Option<ID3D12Debug> = None;
                D3D12GetDebugInterface(&mut layer)
                    .expect("Graphics Tools or explicit Wine opt-out");
                let layer = layer.unwrap();
                layer.EnableDebugLayer();
                layer
                    .cast::<ID3D12Debug1>()
                    .unwrap()
                    .SetEnableGPUBasedValidation(true);
            }
        }
    });
    let host = Host::new(false).expect("hardware D3D12 SM6.6 device");
    let mut caps = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
    // SAFETY: live device and correctly sized query output.
    unsafe {
        host.device
            .CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS1,
                (&raw mut caps).cast(),
                size_of_val(&caps) as u32,
            )
            .unwrap();
        let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
        let adapter: IDXGIAdapter1 = factory
            .EnumAdapterByLuid(host.device.GetAdapterLuid())
            .unwrap();
        let desc = adapter.GetDesc1().unwrap();
        assert_ne!(
            desc.VendorId, 0x1414,
            "hardware diagnostic must not run on WARP"
        );
        eprintln!(
            "adapter vendor={:#x} device={:#x}; lanes=({}, {}); diagnostics={debug}",
            desc.VendorId, desc.DeviceId, caps.WaveLaneCountMin, caps.WaveLaneCountMax
        );
    }
    assert!(caps.WaveOps.as_bool());
    assert_eq!((caps.WaveLaneCountMin, caps.WaveLaneCountMax), (32, 32));
    host
}

fn clear_debug(host: &Host) {
    let q: ID3D12InfoQueue = host.device.cast().unwrap();
    unsafe { q.ClearStoredMessages() };
}
fn check_debug(host: &Host) {
    let q: ID3D12InfoQueue = host.device.cast().unwrap();
    let mut warnings = Vec::new();
    unsafe {
        for i in 0..q.GetNumStoredMessages() {
            let mut size = 0;
            q.GetMessage(i, None, &mut size).unwrap();
            let mut bytes = vec![0usize; size.div_ceil(size_of::<usize>())];
            let message = bytes.as_mut_ptr().cast::<D3D12_MESSAGE>();
            q.GetMessage(i, Some(message), &mut size).unwrap();
            if (*message).Severity.0 <= D3D12_MESSAGE_SEVERITY_WARNING.0 {
                let text = std::ffi::CStr::from_ptr((*message).pDescription.cast())
                    .to_string_lossy()
                    .into_owned();
                if text
                    == "ID3D12Device::CreateCommittedResource: Ignoring InitialState D3D12_RESOURCE_STATE_COPY_DEST. Buffers are effectively created in state D3D12_RESOURCE_STATE_COMMON."
                {
                    eprintln!("KNOWN_HOST_WARNING {text}");
                } else {
                    warnings.push(text);
                }
            }
        }
    }
    assert!(warnings.is_empty(), "D3D12/GBV: {warnings:?}");
}

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
        mov(4, k(if threads == 96 { 11 } else { 0 })),
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
        alu(
            0x1D,
            14,
            k(if !threads.is_multiple_of(64) { 8 } else { 32 }),
            0,
        ),
        alu(0x1A, 14, k(2), 14),
    ]);
    let counter_bytes = (threads.div_ceil(64) * 4).max(8);
    let [op, args] = ds(0x0D, 0, 10, 4);
    words.extend([op | counter_bytes, args]);
    let [op, args] = ds(0x36, 10, 14, 0);
    words.extend([op | counter_bytes, args]);

    if threads > 64 {
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

// Original Dathinel 561503d oracle, unchanged.
fn oracle(threads: u32, thread: u32) -> [u32; OUTPUTS] {
    let wave = thread / 64;
    let lane = thread % 64;
    let lane_sum = |lane| u32::from(threads == 96) * 11 + lane * (1 + (lane % 4) + wave);
    let wave_total = |wave: u32| {
        (0..(threads - wave * 64).min(64))
            .map(|lane| 1 + lane % 4 + wave)
            .sum()
    };
    [
        lane_sum(lane),
        wave_total(wave),
        lane_sum([40, 24][(wave & 1) as usize]),
        lane_sum(if threads == 96 && wave == 1 {
            0
        } else {
            [20, 36][(wave & 1) as usize]
        }),
        wave_total(if threads > 64 { wave ^ 1 } else { wave }),
        lane_sum(lane ^ if !threads.is_multiple_of(64) { 8 } else { 32 }),
        thread,
    ]
}

fn compile_diagnostic(native: &NativeProgram) -> Result<super::HostShader, String> {
    let bytes = crate::dxbc::build(&[]);
    let c = crate::Container::parse(&bytes).map_err(|e| e.to_string())?;
    let (p, st) = crate::range::build(&native.patched_code(), native.wave64, native.vgprs)
        .map_err(|e| e.to_string())?;
    let st = st.map_err(|e| e.to_string())?;
    let res = crate::resource::track(&p, &native.layout, native.stage, Some(&st))
        .map_err(|e| e.to_string())?;
    let io = crate::io::raise(&p, native).map_err(|e| e.to_string())?;
    super::emit_with(
        &c,
        native,
        &p,
        &st,
        &io,
        &res,
        super::Options {
            host_wave32: true,
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())
}

#[test]
#[ignore = "bounded independent compatibility/readback check"]
fn private_lds_halves_compatibility() {
    let host = checked_host();
    let mut failures = 0;
    for threads in [64, 96] {
        let original = guest(threads);
        let p = crate::ir::build(&original.code, true, original.vgprs).unwrap();
        let proof = crate::new354_private_lds::promote(&original, &p).unwrap();
        eprintln!(
            "PROOF addresses={:?} atomic={} publication={} extra-scalars={} extra-vectors={}",
            proof.counter_addresses,
            proof.atomic_offset,
            proof.publication_offset,
            proof.scalar_words,
            proof.vector_words
        );
        let promoted = proof.native;
        for (name, native) in [("original", &original), ("promoted", &promoted)] {
            if std::env::var_os("NEW354_LDS_FOCUS").is_some()
                && (threads != 96 || name != "promoted")
            {
                continue;
            }
            clear_debug(&host);
            let mut shader = match compile_diagnostic(native) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("LDS_HALVES threads={threads} form={name} EMIT_REFUSED {e}");
                    continue;
                }
            };
            super::validator::Validator::load(
                super::validator::Validator::default_path().as_deref(),
            )
            .unwrap()
            .validate(&mut shader.dxil)
            .unwrap();
            let (root, pso) = match host.compute_pipeline(&shader) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("LDS_HALVES threads={threads} form={name} PSO_REFUSED {e}");
                    continue;
                }
            };
            let views = (0..OUTPUTS)
                .map(|_| View::RawUav(threads + GUARD as u32))
                .collect::<Vec<_>>();
            let out = host
                .dispatch(
                    &root,
                    &pso,
                    &Dispatch {
                        roots: vec![(
                            0,
                            Root::Constants(vec![0; shader.user_data_dwords().max(1) as usize]),
                        )],
                        views: &views,
                        samplers: &[],
                        groups: [1, 1, 1],
                    },
                )
                .unwrap();
            assert_eq!(out.len(), OUTPUTS);
            let mut wrong = 0;
            for (slot, data) in out.iter().enumerate() {
                assert_eq!(data.len(), threads as usize + GUARD);
                assert_eq!(&data[threads as usize..], &[0; GUARD]);
                for t in 0..threads {
                    wrong += usize::from(data[t as usize] != oracle(threads, t)[slot]);
                }
                let count = (0..threads)
                    .filter(|&t| data[t as usize] != oracle(threads, t)[slot])
                    .count();
                eprintln!(
                    "LDS_SLOT threads={threads} form={name} slot={slot} wrong={count} first={} expected={} last={} expected-last={}",
                    data[0],
                    oracle(threads, 0)[slot],
                    data[threads as usize - 1],
                    oracle(threads, threads - 1)[slot]
                );
            }
            failures += wrong;
            check_debug(&host);
            eprintln!(
                "LDS_HALVES threads={threads} form={name} EXECUTED compared={} wrong={wrong} guards={}",
                OUTPUTS * threads as usize,
                GUARD * OUTPUTS
            );
        }
    }
    assert_eq!(failures, 0, "admitted forms need correct complete output");
}
