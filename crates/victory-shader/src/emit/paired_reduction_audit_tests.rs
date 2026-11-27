//! Test-only audit of paired 741aa8e against ColorMeBoom's reduction regression.
//! Guest and encoding helpers: ColorMeBoom/VIctory fce45f8 (GPL-3.0-or-later).
//! Partial sizes, differing wave loop lengths and CPU oracle: NEW-354.
use super::host::{Dispatch, Host, Root, View};
use super::validator;
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{Container, FourCC, dxbc::u32_at};
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;
const S_ENDPGM: u32 = 0xBF81_0000;
const S_WAITCNT_LGKM: u32 = 0xBF8C_C07F;
const PROLOGUE: [u32; 5] = [
    0xBE84_0300,
    0xBE85_0380,
    0xF408_0A02,
    0xFA00_0040,
    S_WAITCNT_LGKM,
];
const fn vop1(vdst: u8, op: u8, src0: u16) -> u32 {
    (0x3F << 25) | (vdst as u32) << 17 | (op as u32) << 9 | (src0 as u32 & 0x1FF)
}

const fn vop2(vdst: u8, op: u8, src0: u16, vsrc1: u8) -> u32 {
    (op as u32) << 25 | (vdst as u32) << 17 | (vsrc1 as u32) << 9 | (src0 as u32 & 0x1FF)
}

const fn vop3(vdst: u8, op: u16, src0: u16, src1: u16, src2: u16) -> [u32; 2] {
    [
        0xD400_0000 | (op as u32) << 16 | vdst as u32,
        (src0 as u32 & 0x1FF) | ((src1 as u32 & 0x1FF) << 9) | ((src2 as u32 & 0x1FF) << 18),
    ]
}

const fn sop1(sdst: u8, op: u8, ssrc0: u8) -> u32 {
    0xBE80_0000 | (sdst as u32) << 16 | (op as u32) << 8 | ssrc0 as u32
}

const fn dpp16(vsrc0: u8, ctrl: u16) -> u32 {
    0xFF << 24 | (ctrl as u32) << 8 | vsrc0 as u32
}

const fn store(vdata: u8, offset: u32) -> [u32; 2] {
    [0xE070_1000 | offset, 0x800A_0010 | (vdata as u32) << 8]
}

fn native(words: &[u32], wave64: bool, threads: u32, lds_bytes: u32) -> NativeProgram {
    let range = |kind, table_offset_dwords, dwords| UsedResource {
        kind,
        root: RootType::Table,
        table_offset_dwords,
        lower_bound: 0,
        stride: 0,
        dwords,
        user_data: 0,
        slot: 0,
    };
    NativeProgram {
        stage: HwStage::Cs,
        wave64,
        vgprs: 32,
        layout: vec![RootParam {
            root: RootType::Table,
            table_offset_dwords: 0,
            sgprs: 1,
            base_register: 0,
            user_data: 0,
            slot: 0,
        }],
        used: vec![
            range(ResourceType::Srv, 0, 8),
            range(ResourceType::Srv, 8, 4),
            range(ResourceType::Uav, 16, 4),
        ],
        io: StageIo::Cs(ComputeIo {
            // One user-data SGPR; LDS in blocks of 512 bytes (bits 15-23).
            rsrc2: 1 << 1 | lds_bytes.div_ceil(512) << 15,
            threads: [threads, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

fn sum_in_a_loop_lanes_leave() -> Vec<u32> {
    use crate::ir::EXEC;
    let mut w = PROLOGUE.to_vec();
    w.extend([
        0x3420_0085, // v_lshlrev_b32 v16, 5, v0
        0xD765_0001,
        0x0001_00C1, // v_mbcnt_lo_u32_b32 v1, -1, 0
        0xD766_0001,
        0x0002_02C1,               // v_mbcnt_hi_u32_b32 v1, -1, v1
        vop2(9, 0x1B, 128 + 3, 1), // v_and_b32 v9, 3, v1
        vop2(9, 0x25, 128 + 1, 9), // v_add_nc_u32 v9, 1, v9
        vop1(8, 0x01, 128 + 1),    // v_mov_b32 v8, 1
        vop1(11, 0x01, 128),       // v_mov_b32 v11, 0
        sop1(50, 0x03, 128),       // s_mov_b32 s50, 0
        0xBE9A_047E,               // s_mov_b64 s[26:27], exec
        sop1(52, 0x04, EXEC),      // s_mov_b64 s[52:53], exec
    ]);
    let top = w.len();
    w.push(0x8000_0000 | 2 << 23 | 50 << 16 | (128 + 1) << 8 | 50); // s_add_i32 s50, s50, 1
    w.push(sop1(2, 0x25, 193)); // s_or_saveexec_b64 s[2:3], -1
    w.extend(vop3(2, 0x101, 128, 256 + 8, 2)); // v_cndmask_b32 v2, 0, v8, s[2:3]
    for ctrl in [0xB1, 0x4E, 0x141, 0x140] {
        // v_add_nc_u32 v2, v2, v2 quad_perm:[1,0,3,2], [2,3,0,1], row_half_mirror, row_mirror
        w.extend([vop2(2, 0x25, 0xFA, 2), dpp16(2, ctrl)]);
    }
    w.extend(vop3(3, 0x378, 256 + 2, 128, 128)); // v_permlanex16_b32 v3, v2, 0, 0
    w.push(vop2(2, 0x25, 256 + 3, 2)); // v_add_nc_u32 v2, v3, v2
    w.extend(vop3(4, 0x360, 256 + 2, 128 + 32, 0)); // v_readlane_b32 s4, v2, 32
    w.push(vop2(2, 0x25, 4, 2)); // v_add_nc_u32 v2, s4, v2
    w.extend(vop3(6, 0x360, 256 + 2, 128, 0)); // v_readlane_b32 s6, v2, 0
    w.push(sop1(EXEC, 0x04, 2)); // s_mov_b64 exec, s[2:3]
    w.extend([
        vop2(11, 0x25, 6, 11),                  // v_add_nc_u32 v11, s6, v11
        0x7C00_0000 | 0xD6 << 17 | 9 << 9 | 50, // v_cmpx_ge_u32 s50, v9
        0x8000_0000 | 0x15 << 23 | 52 << 16 | 126 << 8 | 52, // s_andn2_b64 s[52:53], s[52:53], exec
    ]);
    // s_cbranch_scc0 over the two instructions that go round again.
    w.push(0xBF84_0002);
    w.push(sop1(EXEC, 0x04, 52)); // s_mov_b64 exec, s[52:53]
    let back = top as i32 - (w.len() as i32 + 1);
    w.push(0xBF82_0000 | (back as u32 & 0xFFFF)); // s_branch to the loop's top
    w.push(0xBEFE_041A); // s_mov_b64 exec, s[26:27]
    w.extend(store(11, 0));
    w.extend(store(9, 4));
    w.push(S_ENDPGM);
    w
}

fn warnings(queue: &ID3D12InfoQueue) -> Vec<String> {
    // SAFETY: The info queue supplies the size; usize storage aligns each message.
    unsafe {
        (0..queue.GetNumStoredMessages())
            .filter_map(|i| {
                let mut size = 0;
                queue.GetMessage(i, None, &mut size).unwrap();
                let mut data = vec![0usize; size.div_ceil(size_of::<usize>())];
                let ptr = data.as_mut_ptr().cast::<D3D12_MESSAGE>();
                queue.GetMessage(i, Some(ptr), &mut size).unwrap();
                let m = &*ptr;
                (m.Severity.0 <= D3D12_MESSAGE_SEVERITY_WARNING.0).then(|| {
                    std::ffi::CStr::from_ptr(m.pDescription.cast())
                        .to_string_lossy()
                        .into_owned()
                })
            })
            .collect()
    }
}

// Derive expected results from live guest IDs, not shader IR or host wave ops.
fn oracle(threads: u32, thread: u32, distinct: bool) -> [u32; 2] {
    let base = thread / 64 * 64;
    let end = (base + 64).min(threads);
    let rounds = |t: u32| ((t % 64) & 3) + 1 + if distinct { t / 64 } else { 0 };
    let n = rounds(thread);
    let sum = (1..=n)
        .map(|round| (base..end).filter(|&t| rounds(t) >= round).count() as u32)
        .sum();
    [sum, n]
}

#[test]
#[ignore = "requires exclusively wave32, Graphics Tools and DXIL validator; isolated process"]
fn paired_reduction_excludes_absent_and_departed_lanes() {
    // SAFETY: This ignored test is invoked alone, before creating its first device.
    unsafe {
        let mut debug: Option<ID3D12Debug> = None;
        D3D12GetDebugInterface(&mut debug).expect("Graphics Tools");
        let debug = debug.unwrap();
        debug.EnableDebugLayer();
        debug
            .cast::<ID3D12Debug1>()
            .unwrap()
            .SetEnableGPUBasedValidation(true);
    }
    let host = Host::new(false).expect("hardware host");
    let mut caps = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
    // SAFETY: Matching feature-query description and size.
    unsafe {
        host.device
            .CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS1,
                (&raw mut caps).cast(),
                size_of_val(&caps) as u32,
            )
            .unwrap();
    }
    assert!(caps.WaveOps.as_bool());
    assert_eq!((caps.WaveLaneCountMin, caps.WaveLaneCountMax), (32, 32));
    let queue: ID3D12InfoQueue = host.device.cast().unwrap();
    assert!(warnings(&queue).is_empty());
    let mut validator = validator::Validator::load(validator::Validator::default_path().as_deref())
        .expect("DXIL validator");
    let mut all_failures = Vec::new();
    let mut dispatched = 0;
    let mut refused = 0;
    let mut checked = 0;
    let sizes: &[u32] = &[32, 33, 48, 63, 64, 65, 96, 97, 127, 128, 129, 192, 256];
    for distinct in [false, true] {
        for &threads in sizes {
            let mut words = sum_in_a_loop_lanes_leave();
            if distinct {
                let at = words.iter().position(|&w| w == vop1(8, 0x01, 129)).unwrap();
                words.splice(at..at, [vop2(10, 0x16, 134, 0), vop2(9, 0x25, 266, 9)]);
            }
            let guest = native(&words, true, threads, 0);
            let bytes = crate::dxbc::build(&[]);
            let c = Container::parse(&bytes).unwrap();
            let p = crate::ir::build(&guest.code, true, guest.vgprs).expect("IR");
            super::lower::TEST_HOST_WAVE.set(Some(32));
            let result = super::paired_impl(&c, &guest, &p, !threads.is_multiple_of(64));
            super::lower::TEST_HOST_WAVE.set(None);
            let (mut shader, _) = match result {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("ADMISSION threads={threads} distinct={distinct} refused={e}");
                    refused += 1;
                    continue;
                }
            };
            validator
                .validate(&mut shader.dxil)
                .expect("DXIL validation");
            let dxil = Container::parse(&shader.dxil).unwrap();
            let psv = dxil.part(FourCC::PSV0).unwrap();
            assert_eq!(u32_at(psv.data, 40).unwrap(), threads.div_ceil(64) * 32);
            assert_eq!(u32_at(psv.data, 20).unwrap(), 32);
            assert_eq!(u32_at(psv.data, 24).unwrap(), 32);
            let (root, pso) = host.compute_pipeline(&shader).expect("PSO");
            // Table dword 16 names descriptor 2. Two unused UAVs occupy 0/1.
            let views = [
                View::RawUav(8),
                View::RawUav(8),
                View::RawUav(threads * 8 + 8),
            ];
            let data = vec![0; shader.user_data_dwords().max(1) as usize];
            let outputs = host
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
                .expect("dispatch/readback");
            assert_eq!(outputs.len(), 3);
            assert_eq!(outputs[2].len(), (threads * 8 + 8) as usize);
            let mut mismatches = 0;
            for thread in 0..threads {
                let expected = oracle(threads, thread, distinct);
                for (slot, want) in expected.into_iter().enumerate() {
                    let actual = outputs[2][thread as usize * 8 + slot];
                    checked += 1;
                    if actual != want {
                        mismatches += 1;
                        eprintln!(
                            "MISMATCH threads={threads} distinct={distinct} lane={thread} slot={slot} got={actual} expected={want}"
                        );
                    }
                }
                assert_eq!(
                    &outputs[2][thread as usize * 8 + 2..thread as usize * 8 + 8],
                    &[0; 6]
                );
            }
            assert_eq!(&outputs[2][threads as usize * 8..], &[0; 8]);
            assert!(outputs[..2].iter().flatten().all(|&w| w == 0));
            let messages = warnings(&queue);
            assert!(messages.is_empty(), "{messages:#?}");
            eprintln!(
                "RESULT threads={threads} distinct={distinct} translated=true validated=true PSO=true readback=true wrong={mismatches} guards=ok"
            );
            if mismatches != 0 {
                all_failures.push((threads, distinct, mismatches));
            }
            dispatched += 1;
        }
    }
    eprintln!(
        "SUMMARY dispatched={dispatched} refused={refused} checked={checked} failures={all_failures:?}"
    );
    assert_eq!(
        dispatched, 8,
        "all complete-wave controls must actually execute"
    );
    assert_eq!(
        refused, 18,
        "the partial variants retain the existing admission boundary"
    );
    assert!(all_failures.is_empty());
}
