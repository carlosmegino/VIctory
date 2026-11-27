//! Synthetic diagnostics for the exact a9ef792 compiler, not a compiler fix.
//! Lane-loop guest: r4tedx/VIctory e2d3cfd, lane_read_loop_tests.rs (GPL-3.0-or-later).
//! First-lane inputs/reference: Lyrnc's pinned public gist, fetched separately.

use super::host::{Dispatch, Host, Root, View};
use crate::native::{ComputeIo, HwStage, NativeProgram, RootParam, RootType, StageIo};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::*;

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

fn execute(host: &Host, native: &NativeProgram, count: usize) -> Result<Vec<u32>, String> {
    let bytes = crate::dxbc::build(&[]);
    let c = crate::dxbc::Container::parse(&bytes).map_err(|e| e.to_string())?;
    let p =
        crate::ir::build(&native.code, native.wave64, native.vgprs).map_err(|e| e.to_string())?;
    let st = crate::structure::structure(&p).map_err(|e| e.to_string())?;
    let res = crate::resource::track(&p, &native.layout, native.stage, Some(&st))
        .map_err(|e| e.to_string())?;
    let io = crate::io::raise(&p, native).map_err(|e| e.to_string())?;
    let elections: Vec<_> = p
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(b, block)| {
            let st = &st;
            let res = &res;
            block
                .insts
                .iter()
                .enumerate()
                .filter(|(_, inst)| inst.name() == "v_readfirstlane_b32")
                .map(move |(i, inst)| {
                    (
                        inst.machine.offset,
                        st.guards[b][i].is_some(),
                        inst.writes
                            .first()
                            .is_some_and(|w| res.waterfall.reads.contains(&w.1)),
                    )
                })
        })
        .collect();
    eprintln!("first-lane selection (offset, has EXEC guard, waterfall): {elections:?}");
    let mut shader = super::emit_with(
        &c,
        native,
        &p,
        &st,
        &io,
        &res,
        super::Options {
            wave32_only: super::host::wave32_only(&host.device),
            ..Default::default()
        },
    )
    .map_err(|e| format!("EMIT: {e}"))?;
    super::validator::Validator::load(super::validator::Validator::default_path().as_deref())
        .map_err(|e| format!("VALIDATOR: {e}"))?
        .validate(&mut shader.dxil)
        .map_err(|e| format!("INVALID: {e}"))?;
    let module = crate::Container::parse(&shader.dxil).map_err(|e| e.to_string())?;
    let psv = module.part(crate::FourCC::PSV0).ok_or("no PSV0")?.data;
    eprintln!(
        "PSV0 physical={:?} wave={:?}",
        [40, 44, 48].map(|at| crate::dxbc::u32_at(psv, at).unwrap()),
        [20, 24].map(|at| crate::dxbc::u32_at(psv, at).unwrap())
    );
    let (root, pso) = host
        .compute_pipeline(&shader)
        .map_err(|e| format!("PSO: {e}"))?;
    let mut out = host
        .dispatch(
            &root,
            &pso,
            &Dispatch {
                roots: vec![(
                    0,
                    Root::Constants(vec![0; shader.user_data_dwords().max(1) as usize]),
                )],
                views: &[View::RawUav(count as u32)],
                samplers: &[],
                groups: [1, 1, 1],
            },
        )
        .map_err(|e| format!("DISPATCH: {e}"))?;
    assert_eq!(out.len(), 1, "one complete output");
    let out = out.remove(0);
    assert_eq!(out.len(), count, "complete readback required");
    Ok(out)
}

// The original program() function is appended verbatim by the preparation script.

#[test]
#[ignore = "synthetic hardware diagnostic; run as a separate process"]
fn lane_loop_consumers() {
    let host = checked_host();
    let mut wrong = 0;
    let mut refused = 0;
    for (wave, lane, joined) in [(32, 20, false), (64, 40, false), (64, 40, true)] {
        match execute(&host, &program(wave, lane, joined), wave as usize) {
            Ok(out) => {
                let bad: Vec<_> = out
                    .iter()
                    .enumerate()
                    .filter(|(t, got)| **got != *t as u32 + lane)
                    .map(|(t, got)| (t, *got, t as u32 + lane))
                    .collect();
                eprintln!(
                    "wave{wave}, lane{lane}, joined={joined}: {}/{} wrong; {bad:?}",
                    bad.len(),
                    wave
                );
                wrong += bad.len();
            }
            Err(e) => {
                eprintln!("wave{wave}, joined={joined}: REFUSED {e}");
                refused += 1;
            }
        }
    }
    assert_eq!(refused, 0, "admission is separate from returned values");
    assert_eq!(wrong, 0, "preserve the original consumer oracle");
}

#[test]
#[ignore = "requires the separately downloaded, pinned synthetic firstlane inputs"]
fn firstlane_native_oracle() {
    let host = checked_host();
    let input = std::fs::read_to_string(
        std::env::var_os("VICTORY_REPRO_CASES").expect("synthetic cases path"),
    )
    .unwrap();
    let output = std::path::PathBuf::from(
        std::env::var_os("VICTORY_REPRO_OUT").expect("output path outside checkout"),
    );
    let mut rows = Vec::new();
    let mut count = 0;
    let mut refused = 0;
    for line in input.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 3);
        let name = fields[0];
        assert!(
            name.starts_with("firstlane_branch/"),
            "only the scoped synthetic cases"
        );
        let lds: u32 = fields[1].parse().unwrap();
        let words: Vec<u32> = fields[2]
            .split(',')
            .map(|v| u32::from_str_radix(v, 16).unwrap())
            .collect();
        let native = NativeProgram {
            stage: HwStage::Cs,
            wave64: true,
            vgprs: 64,
            layout: vec![RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 0,
                slot: 0,
            }],
            used: vec![crate::native::UsedResource {
                kind: crate::native::ResourceType::Uav,
                root: RootType::Table,
                table_offset_dwords: 0,
                lower_bound: 0,
                stride: 0,
                dwords: 4,
                user_data: 0,
                slot: 0,
            }],
            io: StageIo::Cs(ComputeIo {
                rsrc2: 1 << 1 | lds.div_ceil(512) << 15,
                threads: [64, 1, 1],
            }),
            code: words.iter().flat_map(|v| v.to_le_bytes()).collect(),
        };
        count += 1;
        match execute(&host, &native, 1024) {
            Ok(out) => rows.push(format!(
                "{name}\tOK\t{}",
                out.iter()
                    .map(|v| format!("{v:x}"))
                    .collect::<Vec<_>>()
                    .join(",")
            )),
            Err(e) => {
                rows.push(format!("{name}\tREFUSED\t{}", e.replace(['\n', '\t'], " ")));
                refused += 1;
            }
        }
    }
    assert_eq!(count, 10, "all ten firstlane masks must be attempted");
    std::fs::write(output, rows.join("\n") + "\n").unwrap();
    assert_eq!(
        refused, 0,
        "compare only complete, validated hardware dispatches"
    );
}
fn program(wave: u32, lane: u32, joined_latch: bool) -> crate::NativeProgram {
    let wave64 = wave == 64;
    let k = |n: u32| 128 + n; // inline integer constant
    let (saveexec, and_exec, or_exec) = if wave64 {
        (
            0xBE8A_246A, // s_and_saveexec_b64 s[10:11], vcc
            0x87FE_6A7E, // s_and_b64 exec, exec, vcc
            0x88FE_0A7E, // s_or_b64 exec, exec, s[10:11]
        )
    } else {
        (
            0xBE8A_3C6A, // s_and_saveexec_b32 s10, vcc_lo
            0x877E_6A7E, // s_and_b32 exec_lo, exec_lo, vcc_lo
            0x887E_0A7E, // s_or_b32 exec_lo, exec_lo, s10
        )
    };
    let mut words = vec![
        0xBE82_0300, // s_mov_b32 s2, s0
        0xBE83_0380, // s_mov_b32 s3, 0
        0xF408_0101,
        0xFA00_0000, // s_load_dwordx4 s[4:7], s[2:3], null
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0xBE89_0380, // s_mov_b32 s9, 0
        0x7E04_0280, // v_mov_b32 v2, 0
        0x7D8A_0080, // v_cmp_ne_u32 vcc, 0, v0
        saveexec,
        if joined_latch {
            0xBF88_0009
        } else {
            0xBF88_0006
        }, // s_cbranch_execz end
        // loop:
        0xD760_0009,
        k(lane) << 9 | 0x100, // v_readlane_b32 s9, v0, lane
        0x4A04_0481,          // v_add_nc_u32 v2, 1, v2
        0x7D82_0102,          // v_cmp_lt_u32 vcc, v2, v0
        and_exec,
    ];
    if joined_latch {
        // Keep a second predecessor of the latch in the CFG. The latch's
        // EXEC phi feeds the loop header's loop-carried EXEC phi.
        words.extend([
            0xBF88_0003, // s_cbranch_execz end
            0xBF85_0001, // s_cbranch_scc1 latch (and_exec set SCC)
            0xBEFE_0480, // s_mov_b64 exec, 0, alternate CFG edge
            0xBF82_FFF7, // latch: s_branch loop
        ]);
    } else {
        words.push(0xBF89_FFFA); // s_cbranch_execnz loop
    }
    words.extend([
        // end:
        or_exec,
        0x4A04_0409, // v_add_nc_u32 v2, s9, v2
        0x3406_0082, // v_lshlrev_b32 v3, 2, v0
        0xE070_1000,
        0x8001_0203, // buffer_store_dword v2, v3, s[4:7], 0 offen
        0xBF81_0000, // s_endpgm
    ]);
    crate::NativeProgram {
        stage: crate::HwStage::Cs,
        wave64,
        vgprs: 8,
        layout: vec![RootParam {
            root: RootType::Table,
            table_offset_dwords: 0,
            sgprs: 1,
            base_register: 0,
            user_data: 0,
            slot: 0,
        }],
        used: vec![],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 1 << 1, // one user SGPR (the UAV table)
            threads: [wave, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}
