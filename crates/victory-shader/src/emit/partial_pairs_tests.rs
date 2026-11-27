//! Dathinel (561503d) and PiloteCode (#854) synthetic guest and harness.
//! Includes the bounded-EXEC and guarded-memory variant from NEW-354.

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

fn run_guest(
    dims: [u32; 3],
    native: NativeProgram,
    expected: fn(u32, u32) -> [u32; OUTPUTS],
    check_atomic: bool,
) {
    let threads = dims.into_iter().product::<u32>();
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
    // Use the actual host capability and the runtime's per-shader opt-in.
    super::set_host_max_wave(caps.WaveLaneCountMax);
    let mut shader = super::with_wave_pairs(true, || translate_regular(&native)).unwrap();
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
        "ROUTE: guest={} {dims:?}, physical={}, wave-range={}..{}",
        threads,
        u32_at(psv.data, 40).unwrap(),
        u32_at(psv.data, 20).unwrap(),
        u32_at(psv.data, 24).unwrap()
    );
    let mut views: Vec<_> = (0..OUTPUTS)
        .map(|_| View::RawUav(threads + GUARD as u32))
        .collect();
    if check_atomic {
        views.push(View::RawUav(1));
    }
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
    assert_eq!(out.len(), views.len());
    if check_atomic {
        assert_eq!(
            out[OUTPUTS],
            [threads],
            "one buffer atomic per present lane"
        );
    }
    let mut failures = 0;
    for (slot, words) in out.iter().take(OUTPUTS).enumerate() {
        assert_eq!(words.len(), threads as usize + GUARD);
        let mut count = 0;
        for thread in 0..threads {
            let want = expected(threads, thread)[slot];
            if words[thread as usize] != want {
                count += 1;
            }
        }
        failures += count;
        eprintln!(
            "CHECK: UAV={slot} mismatches={count}/{threads}; t0={} expected={}; last={} expected={}",
            words[0],
            expected(threads, 0)[slot],
            words[(threads - 1) as usize],
            expected(threads, threads - 1)[slot]
        );
        assert_eq!(&words[threads as usize..], &[0; GUARD], "UAV {slot} guard");
    }
    assert_eq!(failures, 0, "all seven UAVs checked; guards intact");
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

fn memory_and_exec_guest() -> (NativeProgram, Vec<u32>) {
    let mut native = guest(97);
    let mut counter = native.used[0];
    counter.table_offset_dwords = OUTPUTS as u32 * 8;
    counter.lower_bound = OUTPUTS as u32;
    native.used.push(counter);
    let mut words = vec![
        alu(0x1B, 2, k(63), 0), // logical lane
        alu(0x25, 4, k(11), 2), // nonzero source
        sop1(4, 22, EXEC),
        sop1(4, EXEC, k(0)),
        0x7E00_0400 | 6 << 17 | v(4), // first lane with EXEC empty
        0xD761_0004,                  // writelane v4, 42, lane 1, ignores EXEC
        k(42) | (k(1) << 9),
        0xD760_0007, // readlane s7, v4, 1, ignores EXEC
        v(4) | (k(1) << 9),
        sop1(4, EXEC, 22),
        mov(10, 6),
        mov(11, 7),
        mov(6, k(63)), // sentinel for lanes left out by direct EXEC = 3
        mov(14, k(4)), // source lane one
        sop1(4, EXEC, k(3)),
    ];
    words.extend(ds(0xB3, 6, 14, 4));
    words.extend([
        sop1(4, EXEC, 22),
        alu(0x1D, 14, k(1), 2),
        alu(0x1A, 14, k(2), 14),
    ]);
    words.extend(ds(0xB3, 9, 14, 4)); // neighbor in the same GFX10 half, or zero
    words.extend([
        mov(14, 255),
        1024, // first byte beyond guest LDS, the private counter slot
    ]);
    words.extend(ds(0x0D, 0, 14, 4)); // guest out-of-bounds store is dropped
    words.extend(ds(0x36, 12, 14, 0)); // guest out-of-bounds read yields zero
    words.extend([
        0xBE85_0380,
        0x3D << 26 | 0x02 << 18 | 8 << 6 | 2,
        (125 << 25) | (OUTPUTS as u32 * 32),
        0xBF8C_C07F,
        mov(13, k(1)),
        0xE0C8_0000,
        0x8002_0D00, // buffer_atomic_add at byte zero, no return
        0xBF8C_0F70, // wait before the inter-wave rendezvous
        0xBF8A_0000,
        0xE030_0000,
        0x8002_0700, // buffer_load_dword v7 at byte zero
        0xBF8C_0F70,
        alu(0x1A, 15, k(2), 0),
    ]);
    for (slot, reg) in [10, 11, 6, 9, 12, 7, 0].into_iter().enumerate() {
        words.extend([
            0x3D << 26 | 0x02 << 18 | 8 << 6 | 2,
            125 << 25 | (slot as u32 * 32),
            0xBF8C_C07F,
            0xE070_1000,
            0x8002_0000 | reg << 8 | 15,
        ]);
    }
    words.push(0xBF81_0000);
    (native, words)
}

fn memory_and_exec_oracle(threads: u32, thread: u32) -> [u32; OUTPUTS] {
    let (wave, lane) = (thread / 64, thread % 64);
    let source = lane ^ 1;
    [
        11,
        42,
        if lane < 2 { 42 } else { 63 },
        if wave * 64 + source < threads {
            if source == 1 { 42 } else { 11 + source }
        } else {
            0
        },
        0,
        threads,
        thread,
    ]
}

#[test]
#[ignore = "requires a wave32 GPU, Graphics Tools, and DXIL validator"]
fn paired_partial_direct_exec_and_memory() {
    let (mut native, words) = memory_and_exec_guest();
    native.code = words.into_iter().flat_map(u32::to_le_bytes).collect();
    run_guest([97, 1, 1], native, memory_and_exec_oracle, true);
}

#[test]
fn wave64_partial_group_rejects_unproven_effects() {
    let (native, words) = memory_and_exec_guest();
    let translate = |mut native: NativeProgram, words: Vec<u32>| {
        native.code = words.into_iter().flat_map(u32::to_le_bytes).collect();
        let was = super::lower::TEST_HOST_WAVE.replace(Some(32));
        let pairs = super::TEST_PAIRS.replace(Some(true));
        let result = translate_static(&native);
        super::TEST_PAIRS.set(pairs);
        super::lower::TEST_HOST_WAVE.set(was);
        result.err().map(|e| e.to_string())
    };
    let mut raw = words.clone();
    raw[3] = sop1(4, EXEC, 193); // -1 enables nonexistent logical lanes
    assert!(
        translate(native.clone(), raw).is_some_and(|e| e.contains("bits outside the guest group"))
    );
    let mut lane = words.clone();
    let readlane = lane.iter().position(|&word| word == 0xD760_0007).unwrap();
    lane[readlane + 1] = v(4) | (k(33) << 9); // source is absent in the final 33-lane wave
    assert!(translate(native.clone(), lane).is_some_and(|e| e.contains("not certified")));
    let mut barrier = words.clone();
    let at = barrier.iter().position(|&w| w == 0xBF8A_0000).unwrap();
    barrier.splice(at..at, [cmp(0xD1, k(32), 2), 0xBF88_0001]);
    assert!(translate(native, barrier).is_some_and(|e| e.contains("no proven group rendezvous")));
}

fn translate_static(native: &NativeProgram) -> Result<super::HostShader, crate::ShaderError> {
    let bytes = crate::dxbc::build(&[]);
    let c = Container::parse(&bytes)?;
    let p = crate::ir::build(&native.code, native.wave64, native.vgprs)?;
    super::paired_impl(&c, native, &p, true).map(|(shader, _)| shader)
}

fn translate_regular(native: &NativeProgram) -> Result<super::HostShader, crate::ShaderError> {
    let bytes = crate::dxbc::build(&[]);
    let c = Container::parse(&bytes)?;
    let p = crate::spill::build(native)?;
    let st = crate::structure::structure(&p)?;
    let io = crate::io::raise(&p, native)?;
    let res = crate::resource::track(&p, &native.layout, native.stage, Some(&st))?;
    super::emit(&c, native, &p, &st, &io, &res)
}

#[test]
fn static_partial_regular_selector_preserves_default_and_supports_explicit_opt_in() {
    let (mut native, words) = memory_and_exec_guest();
    native.code = words.into_iter().flat_map(u32::to_le_bytes).collect();
    let bytes = crate::dxbc::build(&[]);
    let c = Container::parse(&bytes).unwrap();
    let p = crate::spill::build(&native).unwrap();
    let st = crate::structure::structure(&p).unwrap();
    let io = crate::io::raise(&p, &native).unwrap();
    let res = crate::resource::track(&p, &native.layout, native.stage, Some(&st)).unwrap();
    super::lower::TEST_HOST_WAVE.set(Some(32));
    super::TEST_PAIRS.set(Some(true));
    let previous = super::emit_legacy(&c, &native, &p, &st, &io, &res);
    assert!(!super::needs_a_wave32_form(&previous));
    let default = translate_regular(&native).unwrap();
    assert_eq!(default.dxil, previous.unwrap().dxil);
    let listed = super::with_wave_pairs(true, || translate_regular(&native)).unwrap();
    assert_eq!(listed.dxil, translate_static(&native).unwrap().dxil);
    assert_ne!(listed.dxil, default.dxil);

    let unproved = guest(96);
    let default = translate_regular(&unproved);
    let listed = super::with_wave_pairs(true, || translate_regular(&unproved));
    match (default, listed) {
        (Ok(a), Ok(b)) => assert_eq!(a.dxil, b.dxil),
        (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string()),
        _ => panic!("a rejected opt-in replaced the old route"),
    }
    super::TEST_PAIRS.set(None);
    super::lower::TEST_HOST_WAVE.set(None);
}

#[test]
fn static_partial_keeps_implicit_lds_barriers_out_of_divergent_loops() {
    let mut native = guest(96);
    let mut words: Vec<_> = native
        .code
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect();
    let add = ds(0, 0, 11, 13)[0];
    let slot = words
        .iter_mut()
        .find(|w| **w == add)
        .expect("LDS operation in the loop");
    *slot = ds(0x08, 0, 11, 13)[0]; // paired admits ds_max_u32 outside a divergent loop
    native.code = words.into_iter().flat_map(u32::to_le_bytes).collect();
    let was = super::lower::TEST_HOST_WAVE.replace(Some(32));
    let result = translate_static(&native);
    super::lower::TEST_HOST_WAVE.set(was);
    assert!(
        matches!(result, Err(crate::ShaderError::Unsupported { what, .. })
        if what == "partial paired barrier has no proven group rendezvous")
    );
}

#[test]
#[ignore = "local archive manifest with authorized inputs outside the repository"]
fn static_partial_archive_preserves_the_existing_route() {
    use crate::{io, resource, structure};
    let manifest = std::env::var_os("VICTORY_LOCAL_PARTIAL_AUDIT").expect("local manifest path");
    let items = std::fs::read_to_string(manifest).unwrap();
    super::lower::TEST_HOST_WAVE.set(Some(32));
    super::TEST_PAIRS.set(Some(true));
    let mut partial = 0;
    let mut complete = 0;
    let mut previously_usable = 0;
    let mut preserved = 0;
    let mut newly_usable = 0;
    let mut outside_container_support = 0;
    let mut earlier_stage_failure = 0;
    for (line, file) in items.lines().enumerate() {
        let bytes = std::fs::read(file).unwrap();
        let c = Container::parse(&bytes).unwrap();
        let native = c.part(FourCC::DXIL).and_then(|part| {
            crate::native_programs(part.data)
                .ok()?
                .into_iter()
                .find(|p| p.stage == HwStage::Cs)
        });
        let Some(native) = native else {
            outside_container_support += 1;
            continue;
        };
        let StageIo::Cs(cs) = native.io else {
            unreachable!()
        };
        let threads: u32 = cs.threads.into_iter().product();
        if threads.is_multiple_of(64) {
            complete += 1;
        } else {
            partial += 1;
        }
        let prepared = (|| {
            let p = crate::spill::build(&native)?;
            let st = structure::structure(&p)?;
            let io = io::raise(&p, &native)?;
            let res = resource::track(&p, &native.layout, native.stage, Some(&st))?;
            Ok::<_, crate::ShaderError>((p, st, io, res))
        })();
        let Ok((p, st, io, res)) = prepared else {
            earlier_stage_failure += 1;
            continue;
        };
        let previous = super::emit_legacy(&c, &native, &p, &st, &io, &res);
        let next = super::emit_any(&c, &native, &p, &st, &io, &res);
        let usable = |r: &Result<super::HostShader, crate::ShaderError>| {
            r.is_ok() && !super::needs_a_wave32_form(r)
        };
        if usable(&previous) {
            previously_usable += 1;
            let previous = previous.unwrap();
            let next = next.expect("old runnable translation must survive");
            assert!(
                previous.dxil == next.dxil,
                "old runnable case {line} changed"
            );
            preserved += 1;
        } else if usable(&next) {
            newly_usable += 1;
        }
    }
    eprintln!(
        "ARCHIVE partial={partial} complete={complete} outside_container_support={outside_container_support} earlier_stage_failure={earlier_stage_failure} prior={previously_usable} unchanged={preserved} rescued={newly_usable}"
    );
    assert_eq!(
        partial + complete + outside_container_support,
        items.lines().count()
    );
}
// Test-only promotion witness for Dathinel's exact wave-private counter.
// This is not an optimizer for arbitrary guest LDS accesses.
fn promoted_counter_guest(threads: u32) -> NativeProgram {
    let mut native = guest(threads);
    let mut words: Vec<u32> = native
        .code
        .as_chunks::<4>()
        .0
        .iter()
        .copied()
        .map(u32::from_le_bytes)
        .collect();
    // v11 = 4 * (tid >> 6), and the invariant addend v13 is exactly 1.
    assert_eq!(
        &words[..3],
        &[
            alu(0x16, 1, k(6), 0),
            alu(0x1B, 2, k(63), 0),
            alu(0x1A, 11, k(2), 1)
        ]
    );
    assert_eq!(words.iter().filter(|&&w| w == mov(13, k(1))).count(), 1);
    let add = ds(0, 0, 11, 13);
    let adds: Vec<_> = words
        .windows(2)
        .enumerate()
        .filter(|(_, w)| *w == add)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(adds.len(), 1);
    let branch = words
        .iter()
        .position(|w| w & 0xFFFF_0000 == 0xBF89_0000)
        .unwrap();
    let top = ((branch + 1) as isize + (words[branch] as i16) as isize) as usize;
    assert_eq!(words[top], alu(0x25, 4, v(2), 4));
    assert!(top < adds[0] && adds[0] < branch);
    // No other LDS instruction occurs in this loop and no atomic result is read.
    let decoded = crate::gfx10::decode(&native.code).unwrap();
    let inside: Vec<_> = decoded
        .iter()
        .filter(|i| {
            i.offset >= top * 4
                && i.offset < branch * 4
                && matches!(i.fields(true), crate::gfx10::Fields::Ds { .. })
        })
        .collect();
    assert_eq!(inside.len(), 1);
    assert_eq!(inside[0].name(), Some("ds_add_u32"));
    // Every original atomic contribution is counted, modulo 2^32. Scalar
    // state is per logical wave in paired; there is no group sync in the loop.
    words.splice(
        adds[0]..adds[0] + 2,
        [sop1(0x10, 28, EXEC), 0x8000_0000 | 30 << 16 | 28 << 8 | 30],
    );
    words.insert(top, sop1(3, 30, k(0))); // s30 counter; existing backedge skips this initializer
    let read = ds(0x36, 5, 11, 0);
    let publish = words.windows(2).position(|w| w == read).unwrap();
    let mut materialize = vec![sop1(4, 32, EXEC), cmp(0xD2, k(0), 2), mov(12, 30)];
    materialize.extend(ds(0x0D, 0, 11, 12));
    materialize.push(sop1(4, EXEC, 32));
    words.splice(publish..publish, materialize);
    native.code = words.into_iter().flat_map(u32::to_le_bytes).collect();
    native
}

#[test]
fn wave_private_counter_promotion_preserves_wrapping_contributions() {
    for threads in [1u32, 32, 33, 64, 96, 97, 128, 256, 1023] {
        let waves = threads.div_ceil(64);
        let mut memory = vec![u32::MAX - 37; waves as usize];
        let mut registers = memory.clone();
        for wave in 0..waves {
            let end = (wave * 64 + 64).min(threads);
            for round in 0..(4 + wave) {
                let live = (wave * 64..end).filter(|lane| round < 1 + (lane & 3) + wave);
                let mut contribution = 0u32;
                for lane in live {
                    let value = lane.wrapping_mul(0x9E37_79B9).wrapping_add(1);
                    memory[wave as usize] = memory[wave as usize].wrapping_add(value);
                    contribution = contribution.wrapping_add(value);
                }
                registers[wave as usize] = registers[wave as usize].wrapping_add(contribution);
            }
        }
        assert_eq!(memory, registers);
    }
    for threads in [64, 96] {
        let n = promoted_counter_guest(threads);
        let decoded = crate::gfx10::decode(&n.code).unwrap();
        assert!(!decoded.iter().any(|i| i.name() == Some("ds_add_u32")));
        let branch = decoded
            .iter()
            .find(|i| i.name() == Some("s_cbranch_execnz"))
            .unwrap();
        let target = branch.offset as isize + 4 + (branch.words[0] as i16) as isize * 4;
        let top = decoded
            .iter()
            .find(|i| i.offset as isize == target)
            .unwrap();
        assert_eq!(top.words, [alu(0x25, 4, v(2), 4)]);
        let bytes = crate::dxbc::build(&[]);
        let c = Container::parse(&bytes).unwrap();
        let p = crate::ir::build(&n.code, true, n.vgprs).unwrap();
        let old = super::lower::TEST_HOST_WAVE.replace(Some(32));
        let result = super::paired_impl(&c, &n, &p, !threads.is_multiple_of(64));
        super::lower::TEST_HOST_WAVE.set(old);
        result.expect("counter promotion removes the loop's only implicit group rendezvous");
        eprintln!("PROMOTION_ADMISSION threads={threads}: exact paired lowering admitted");
    }
}

#[test]
#[ignore = "isolated wave32 GPU test with Graphics Tools and DXIL validator"]
fn paired_wave_private_counter_promotion_matches_all_outputs() {
    // A new 96-thread witness on 741aa8e, not the historical b3060be result.
    run_guest([96, 1, 1], promoted_counter_guest(96), oracle, false);
}

fn analyzed_counter_guest(native: &NativeProgram) -> super::private_lds::Promotion {
    let p = crate::ir::build(&native.code, true, native.vgprs).unwrap();
    super::private_lds::promote(native, &p).unwrap_or_else(|why| {
        panic!(
            "promotion refused: {why}; blocks={:#?}",
            p.blocks
                .iter()
                .map(|b| (b.offset, &b.preds, b.exit))
                .collect::<Vec<_>>()
        )
    })
}

#[test]
fn private_lds_analysis_accepts_original_and_rejects_aliases() {
    for threads in [64, 96, 97, 128, 256] {
        let native = guest(threads);
        let before = native.code.clone();
        let proof = analyzed_counter_guest(&native);
        assert_eq!(native.code, before, "original input is immutable");
        assert_eq!(
            proof.counter_addresses,
            (0..threads.div_ceil(64)).map(|w| w * 4).collect::<Vec<_>>()
        );
        assert_eq!((proof.scalar_words, proof.vector_words), (6, 1));
        assert!(proof.atomic_offset < proof.publication_offset);
        let decoded = crate::gfx10::decode(&proof.native.code).unwrap();
        assert!(!decoded.iter().any(|i| i.name() == Some("ds_add_u32")));
        crate::ir::build(&proof.native.code, true, proof.native.vgprs).unwrap();
    }
    let original = guest(96);
    let change = |from: &[u32], to: &[u32]| {
        let mut n = original.clone();
        let mut words: Vec<_> = n
            .code
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .map(u32::from_le_bytes)
            .collect();
        let at = words.windows(from.len()).position(|w| w == from).unwrap();
        words.splice(at..at + from.len(), to.iter().copied());
        n.code = words.into_iter().flat_map(u32::to_le_bytes).collect();
        n
    };
    let cases = [
        (
            "waves alias",
            change(&[alu(0x1A, 11, k(2), 1)], &[mov(11, k(0))]),
        ),
        (
            "unknown indirect address",
            change(&[alu(0x1A, 11, k(2), 1)], &[mov(11, v(15))]),
        ),
        (
            "lane-private words are not one wave counter",
            change(&[alu(0x1A, 11, k(2), 1)], &[alu(0x1A, 11, k(2), 0)]),
        ),
        (
            "non-unit atomic",
            change(&[mov(13, k(1))], &[mov(13, k(2))]),
        ),
        (
            "nonzero initialization",
            change(&[mov(12, k(0))], &[mov(12, k(1))]),
        ),
        (
            "scratch aliases counter",
            change(&[ds(0x0D, 0, 10, 4)[0] | 8], &[ds(0x0D, 0, 10, 4)[0]]),
        ),
        (
            "returning atomic",
            change(&ds(0, 0, 11, 13), &ds(0x20, 12, 11, 13)),
        ),
        (
            "GDS",
            change(
                &ds(0, 0, 11, 13),
                &[ds(0, 0, 11, 13)[0] | (1 << 17), ds(0, 0, 11, 13)[1]],
            ),
        ),
        (
            "loop-varying address",
            change(&[alu(0x25, 4, v(2), 4)], &[alu(0x25, 11, k(1), 11)]),
        ),
        (
            "missing peer barrier",
            change(&[0xBF8A_0000], &[0xBF80_0000]),
        ),
        (
            "uninitialized lane zero",
            change(&[cmp(0xD2, k(0), 2)], &[cmp(0xD2, k(1), 2)]),
        ),
        (
            "resurrect missing EXEC lanes",
            change(
                &[0x8000_0000 | 0x0F << 23 | EXEC << 16 | VCC << 8 | EXEC],
                &[0x8000_0000 | 0x11 << 23 | EXEC << 16 | 193 << 8 | EXEC],
            ),
        ),
        ("live SCC", {
            let mut n = change(
                &[0x8000_0000 | 0x0F << 23 | EXEC << 16 | VCC << 8 | EXEC],
                &[sop1(4, EXEC, EXEC)],
            );
            let mut words: Vec<_> = n
                .code
                .as_chunks::<4>()
                .0
                .iter()
                .copied()
                .map(u32::from_le_bytes)
                .collect();
            let branch = words.iter_mut().find(|w| **w == 0xBF89_FFF9).unwrap();
            *branch = 0xBF85_FFF9;
            n.code = words.into_iter().flat_map(u32::to_le_bytes).collect();
            n
        }),
    ];
    for (label, n) in cases {
        let p = crate::ir::build(&n.code, true, n.vgprs).unwrap();
        let result = super::private_lds::promote(&n, &p);
        assert!(result.is_err(), "must reject {label}");
        eprintln!("PRIVATE_LDS_REFUSAL {label}: {}", result.err().unwrap());
    }
    let variant = indirect_counter_guest();
    analyzed_counter_guest(&variant);
}

fn indirect_counter_guest() -> NativeProgram {
    let mut n = guest(96);
    let mut words: Vec<_> = n
        .code
        .as_chunks::<4>()
        .0
        .iter()
        .copied()
        .map(u32::from_le_bytes)
        .collect();
    let at = words
        .windows(2)
        .position(|w| w == ds(0, 0, 11, 13))
        .unwrap();
    words[at + 1] = ds(0, 0, 15, 13)[1];
    let mask = words
        .iter_mut()
        .find(|w| **w == alu(0x1B, 3, k(3), 2))
        .unwrap();
    *mask = alu(0x1B, 3, k(5), 2);
    // A register copy carries the indirect address. Both ends of the loop's
    // backedge move by one dword, so its original relative offset is retained.
    words.insert(3, mov(15, v(11)));
    n.code = words.into_iter().flat_map(u32::to_le_bytes).collect();
    n
}

fn indirect_counter_oracle(threads: u32, thread: u32) -> [u32; OUTPUTS] {
    let wave = thread / 64;
    let lane = thread % 64;
    let value = |lane| 11 + lane * (1 + (lane & 5) + wave);
    let total = |wave: u32| {
        (0..(threads - wave * 64).min(64))
            .map(|lane| 1 + (lane & 5) + wave)
            .sum()
    };
    [
        value(lane),
        total(wave),
        value([40, 24][wave as usize]),
        value(if wave == 1 { 0 } else { 20 }),
        total(wave ^ 1),
        value(lane ^ 8),
        thread,
    ]
}

#[test]
fn private_lds_analysis_costs_are_not_hardware_occupancy() {
    let native = guest(96);
    let proof = analyzed_counter_guest(&native);
    let old = super::lower::TEST_HOST_WAVE.replace(Some(32));
    assert!(
        translate_static(&native).is_err(),
        "unoptimized program remains refused"
    );
    let shader = translate_static(&proof.native).unwrap();
    super::lower::TEST_HOST_WAVE.set(old);
    let original = crate::gfx10::decode(&native.code).unwrap();
    let transformed = crate::gfx10::decode(&proof.native.code).unwrap();
    for code in [&original, &transformed] {
        assert_eq!(
            code.iter()
                .filter(|i| i.name() == Some("s_barrier"))
                .count(),
            1
        );
    }
    assert_eq!(
        transformed
            .iter()
            .filter(|i| i.name() == Some("ds_add_u32"))
            .count(),
        0
    );
    // Count actual per-lane guest contributions using the independent oracle.
    let atomics: u32 = [0, 64].into_iter().map(|t| oracle(96, t)[1]).sum();
    let original_lds = 2 + atomics + 4 * 96;
    let promoted_lds = 2 + 2 + 4 * 96;
    assert_eq!((atomics, original_lds, promoted_lds), (272, 658, 388));
    eprintln!(
        "PRIVATE_LDS_COST guest-dynamic atomics=272->0 LDS-accesses=658->388 explicit-barriers=1->1 allocated-extra-scalar-words=6 allocated-extra-vector-words=1 physical-registers=unmeasured occupancy=unmeasured"
    );
    if let Some(census) = super::census::take(&shader.dxil) {
        for (kind, count) in super::census::KINDS.into_iter().zip(census.counts) {
            if [
                "total",
                "lds_load",
                "lds_store",
                "atomic",
                "barrier",
                "phi",
                "wave_op",
                "blocks",
            ]
            .contains(&kind)
            {
                eprintln!("PRIVATE_LDS_DXIL {kind}={count}");
            }
        }
    }
}

#[test]
#[ignore = "isolated Wave32 hardware test with Graphics Tools and DXIL validator"]
fn paired_analyzed_original_96_matches_all_outputs() {
    let original = guest(96);
    let before = original.code.clone();
    let proof = analyzed_counter_guest(&original);
    assert_eq!(original.code, before);
    eprintln!(
        "PRIVATE_LDS original96 counter-addresses={:?} extra-scalars={} extra-vectors={} atomic={} publication={}",
        proof.counter_addresses,
        proof.scalar_words,
        proof.vector_words,
        proof.atomic_offset,
        proof.publication_offset
    );
    run_guest([96, 1, 1], proof.native, oracle, false);
    let indirect = indirect_counter_guest();
    let proof = analyzed_counter_guest(&indirect);
    run_guest([96, 1, 1], proof.native, indirect_counter_oracle, false);
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
