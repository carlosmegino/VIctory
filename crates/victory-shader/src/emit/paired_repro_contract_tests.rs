//! Test-only API/Wine adaptation of #3180 1625d1c; guest and oracle unchanged.
//! Synthetic two-wave contract for Marvisak/VIctory#2688. The LDS loop
//! is adapted from PiloteCode's GPL-3.0 fixture in fxceless/victory-fork#854.

use super::host::{Dispatch, Host, Root, View};
use super::{HostShader, validator};
use crate::ShaderError;
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

const EXEC: u32 = 126;
const VCC: u32 = 106;
const GUARD: usize = 8;
const OUTPUTS: usize = 7;
const WAIT_LGKM: u32 = 0xBF8C_C07F; // s_waitcnt lgkmcnt(0)

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
        WAIT_LGKM, // finish initialization before any lane adds to the counter
        sop1(4, EXEC, 22),
        alu(0x1B, 3, k(3), 2),
        alu(0x25, 3, k(1), 3),
        alu(0x25, 3, v(1), 3), // rounds = 1 + (lane & 3) + wave
        mov(4, k(0)),
        mov(13, k(1)),
        sop1(4, 20, EXEC),
    ]);
    let loop_start = words.len();
    words.push(alu(0x25, 4, v(2), 4)); // loop: accumulate the lane number
    words.extend(ds(0, 0, 11, 13)); // ds_add_u32: add one per participating lane
    words.extend([
        WAIT_LGKM,            // finish this iteration's atomics before narrowing EXEC
        alu(0x25, 3, 193, 3), // decrement remaining rounds
        cmp(0xC5, k(0), 3),
        0x8000_0000 | 0x0F << 23 | EXEC << 16 | VCC << 8 | EXEC, // s_and_b64 exec, exec, vcc
    ]);
    // SOPP offsets count dwords from the instruction following the branch.
    let back = i16::try_from(loop_start as isize - (words.len() + 1) as isize).unwrap();
    words.push(0xBF89_0000 | u32::from(back as u16)); // s_cbranch_execnz loop
    words.push(sop1(4, EXEC, 20));
    words.extend(ds(0x36, 5, 11, 0)); // v5: total from own wave
    words.push(WAIT_LGKM); // v5 must be ready before the 64-thread path copies it

    // Different EXEC masks select a source above 32 and below 32 in each wave.
    for (threshold, shift, scalar, dest) in [(39, 3, 6, 6), (19, 2, 7, 9)] {
        words.extend([
            alu(0x1A, 14, k(shift), 1),
            alu(0x25, 14, k(threshold), 14),
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
    let [op, args] = ds(0x0D, 0, 10, 4);
    words.extend([op | 8, args]);
    words.push(WAIT_LGKM); // make every lane's write visible to its opposite half
    let [op, args] = ds(0x36, 10, 14, 0);
    words.extend([op | 8, args]);
    words.push(WAIT_LGKM); // finish all outstanding LDS before the group barrier

    if threads == 128 {
        words.extend([0xBF8A_0000, alu(0x1D, 14, k(4), 11)]); // s_barrier, then other wave's LDS
        words.extend(ds(0x36, 7, 14, 0));
        words.push(WAIT_LGKM); // peer counter ready before any output consumes v7
    } else {
        words.push(mov(7, v(5)));
    }
    words.extend([alu(0x1A, 15, k(2), 0), 0xBE85_0380]); // output offset and table address high
    for (slot, reg) in [4, 5, 6, 9, 7, 10, 0].into_iter().enumerate() {
        words.extend([
            0x3D << 26 | 0x02 << 18 | 8 << 6 | 2, // s_load_dwordx4 s[8:11], s[4:5]
            125 << 25 | (slot as u32 * 32),
            WAIT_LGKM, // descriptor ready before the buffer store
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
            rsrc2: 5 << 1 | 2 << 15, // s0..s4 and 1 KiB of LDS
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
        lane_sum(40 + 8 * wave),
        lane_sum(20 + 4 * wave),
        wave_total(if threads == 128 { 1 - wave } else { wave }),
        lane_sum(lane ^ 32),
        thread,
    ]
}

#[test]
fn guest_lds_dependencies_and_branches() {
    use crate::gfx10::{Encoding, decode};
    for threads in [64, 128] {
        let native = guest(threads);
        let code = decode(&native.code).expect("decode complete synthetic guest");
        assert_eq!(
            code.iter().map(|i| i.words.len() * 4).sum::<usize>(),
            native.code.len()
        );
        for pair in code.windows(2) {
            if matches!(pair[0].encoding, Encoding::Ds | Encoding::Smem) {
                assert_eq!(
                    pair[1].words,
                    [WAIT_LGKM],
                    "pending LDS/SMEM at {}",
                    pair[0].offset
                );
            }
            if pair[1].name() == Some("s_barrier") {
                assert_eq!(pair[0].words, [WAIT_LGKM], "wait before group barrier");
            }
        }
        let branches: Vec<_> = code
            .iter()
            .filter(|i| i.name() == Some("s_cbranch_execnz"))
            .collect();
        assert_eq!(branches.len(), 1);
        let branch = branches[0];
        let delta = i32::from(branch.words[0] as i16) * 4;
        let target = (branch.offset as i32 + 4 + delta) as usize;
        let start = code
            .iter()
            .position(|i| i.offset == target)
            .expect("branch targets an instruction boundary");
        assert_eq!(
            code[start].words,
            [alu(0x25, 4, v(2), 4)],
            "loop accumulation"
        );
        assert_eq!(code[start + 1].name(), Some("ds_add_u32"));
        assert_eq!(
            code.iter()
                .filter(|i| i.name() == Some("s_barrier"))
                .count(),
            usize::from(threads == 128)
        );
        eprintln!(
            "{threads} threads: branch {} -> {target} ({delta} bytes from next PC), all LDS/SMEM producers waited, barrier audited",
            branch.offset
        );
    }
}

fn translate(native: &NativeProgram, host: &Host) -> Result<HostShader, ShaderError> {
    let bytes = crate::native::tests::compute_uav_container(native);
    super::translate_with(
        &bytes,
        super::Options {
            wave32_only: super::host::wave32_only(&host.device),
            ..Default::default()
        },
    )
}

fn debug_warnings(queue: Option<&ID3D12InfoQueue>) -> Vec<String> {
    let Some(queue) = queue else {
        return Vec::new();
    };
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
    // Hold the parent's shared hardware lock throughout the child's lifetime,
    // including when other GPU tests run with --include-ignored.
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let test = format!("emit::paired_repro_contract_tests::wave64_2688_group_{threads}");
    const CHILD: &str = "VICTORY_PAIRED_REPRO_CHILD";
    if std::env::var(CHILD).as_deref() != Ok(test.as_str()) {
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                &test,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, &test)
            .status()
            .expect("start isolated GPU validation test");
        assert!(status.success(), "isolated {test}: {status}");
        return;
    }

    let debug_enabled = std::env::var("VICTORY_REPRO_NO_DEBUG").as_deref() != Ok("1");
    if debug_enabled {
        // SAFETY: this fresh child has not created a device yet.
        unsafe {
            let mut debug: Option<ID3D12Debug> = None;
            D3D12GetDebugInterface(&mut debug).expect("D3D12 Graphics Tools");
            let debug = debug.expect("D3D12 debug interface");
            debug.EnableDebugLayer();
            debug
                .cast::<ID3D12Debug1>()
                .unwrap()
                .SetEnableGPUBasedValidation(true);
        }
    }
    let host = Host::new(false).expect("selected D3D12 adapter");
    let queue: Option<ID3D12InfoQueue> =
        debug_enabled.then(|| host.device.cast().expect("D3D12 info queue"));
    let warnings = debug_warnings(queue.as_ref());
    assert!(warnings.is_empty(), "D3D12 initialization: {warnings:#?}");
    let mut caps = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
    // SAFETY: The feature query writes only into the matching D3D12 description.
    unsafe {
        host.device
            .CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS1,
                (&raw mut caps).cast(),
                size_of_val(&caps) as u32,
            )
            .expect("wave capabilities");
    }
    if !caps.WaveOps.as_bool() || (caps.WaveLaneCountMin, caps.WaveLaneCountMax) != (32, 32) {
        eprintln!(
            "SKIP {test}: requires exclusively wave32; WaveOps={}, lanes=({}, {}), VICTORY_WARP={:?}",
            caps.WaveOps.as_bool(),
            caps.WaveLaneCountMin,
            caps.WaveLaneCountMax,
            std::env::var_os("VICTORY_WARP")
        );
        return;
    }
    eprintln!("{test}: exclusively wave32; native D3D12/GBV diagnostics enabled={debug_enabled}");
    let mut shader = translate(&guest(threads), &host).expect("translate synthetic wave64");
    validator::Validator::load(validator::Validator::default_path().as_deref())
        .expect("DXIL validator")
        .validate(&mut shader.dxil)
        .expect("validate synthetic DXIL");
    let module = crate::Container::parse(&shader.dxil).expect("emitted DXIL");
    let psv = module.part(crate::FourCC::PSV0).expect("PSV0").data;
    let physical = [40, 44, 48].map(|at| crate::dxbc::u32_at(psv, at).unwrap());
    let lanes = [20, 24].map(|at| crate::dxbc::u32_at(psv, at).unwrap());
    assert_eq!(physical, [threads / 2, 1, 1], "paired physical group");
    assert_eq!(lanes, [32, 32], "required physical wave size");
    eprintln!("paired PSV0: physical={physical:?}, wave={lanes:?}");
    let (root, pso) = host.compute_pipeline(&shader).unwrap_or_else(|error| {
        panic!(
            "compute PSO: {error}; D3D12 validation: {:#?}",
            debug_warnings(queue.as_ref())
        );
    });
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
        .unwrap_or_else(|error| {
            panic!(
                "dispatch and readback: {error}; D3D12 validation: {:#?}",
                debug_warnings(queue.as_ref())
            )
        });
    let warnings = debug_warnings(queue.as_ref());
    let mut failures = warnings.clone();
    assert_eq!(out.len(), OUTPUTS);
    for (slot, words) in out.iter().enumerate() {
        assert_eq!(words.len(), threads as usize + GUARD);
        let mismatches: Vec<_> = (0..threads)
            .filter_map(|thread| {
                let got = words[thread as usize];
                let expected = oracle(threads, thread)[slot];
                (got != expected).then_some((thread, got, expected))
            })
            .collect();
        if let Some((thread, got, expected)) = mismatches.first() {
            failures.push(format!(
                "UAV {slot}, thread {thread}: got {got}, expected {expected} ({} mismatches)",
                mismatches.len()
            ));
        }
        let guards_ok = words[threads as usize..] == [0; GUARD];
        if !guards_ok {
            failures.push(format!(
                "UAV {slot} guard: {:?}",
                &words[threads as usize..]
            ));
        }
        eprintln!(
            "UAV {slot}: {threads} lanes checked, {} mismatches, {GUARD} canaries intact={guards_ok}",
            mismatches.len()
        );
    }
    eprintln!("D3D12 diagnostics checked={debug_enabled}, warnings/errors: {warnings:#?}");
    assert!(
        failures.is_empty(),
        "readback/validation failures: {failures:#?}"
    );
    eprintln!(
        "{threads} guest threads: {OUTPUTS} UAVs match CPU, guards intact; native diagnostics checked={debug_enabled}"
    );
}

// Companion harness only: the backend remains exactly a9ef792.
#[test]
#[ignore = "requires exclusively wave32 and DXIL validator; Graphics Tools unless explicit Wine diagnostics opt-out"]
fn wave64_2688_group_64() {
    run(64);
}

#[test]
#[ignore = "requires exclusively wave32 and DXIL validator; Graphics Tools unless explicit Wine diagnostics opt-out"]
fn wave64_2688_group_128() {
    run(128);
}
