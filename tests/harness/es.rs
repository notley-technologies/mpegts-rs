//! Synthetic H.264, H.265 and MPEG-2 video elementary streams: access
//! units with syntactically valid parameter sets and slice headers (so
//! FFmpeg's `trace_headers` bitstream filter parses them), access unit
//! delimiters, SEI user data, B-frames, and opaque slice data.

/// The user data identifier of the "other" SEI message in the input.
pub const OTHER_UUID: [u8; 16] = *b"other-user-data!";
/// ST 0604 identifiers (st1402-time.md 3.8): H.264 and the H.265
/// microseconds UUID.
pub const MISP_H264: [u8; 16] = *b"MISPmicrosectime";
pub const MISP_H265: [u8; 16] = [
    0xA8, 0x68, 0x7D, 0xD4, 0xD7, 0x59, 0x37, 0x58, 0xA5, 0xCE, 0xF0, 0x33, 0x8B, 0x65, 0x45, 0xF1,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Video {
    H264,
    H265,
    Mpeg2,
}

impl Video {
    pub fn stream_type(self) -> u8 {
        match self {
            Video::H264 => 0x1B,
            Video::H265 => 0x24,
            Video::Mpeg2 => 0x02,
        }
    }

    pub fn misp_uuid(self) -> [u8; 16] {
        match self {
            Video::H265 => MISP_H265,
            _ => MISP_H264,
        }
    }
}

/// What each access unit holds.
#[derive(Debug, Clone, Copy)]
pub struct EsConfig {
    pub video: Video,
    /// A user data message with [`OTHER_UUID`] in the prefix.
    pub other: bool,
    /// An ST 0604-like message (status and modified time stamp, 12 bytes)
    /// with the codec's MISB identifier: in its own SEI NAL unit, or as the
    /// second message of the "other" one when `misp_second`.
    pub misp: bool,
    pub misp_second: bool,
    /// H.264: a prefix NAL unit (type 14) right before the slice.
    pub prefix_nal: bool,
    /// Slice data bytes when no length is asked for.
    pub slice_bytes: usize,
}

impl EsConfig {
    pub fn new(video: Video) -> Self {
        EsConfig {
            video,
            other: false,
            misp: false,
            misp_second: false,
            prefix_nal: false,
            slice_bytes: 600,
        }
    }
}

/// ST 0604 payload after the identifier: Time Status and the modified
/// time stamp (st1402-time.md 3.8, R-ST1402TIME-72).
pub fn st0604_payload(status: u8, time: u64) -> Vec<u8> {
    let t = time.to_be_bytes();
    vec![status, t[0], t[1], 0xFF, t[2], t[3], 0xFF, t[4], t[5], 0xFF, t[6], t[7]]
}

/// MISP time of a frame in these streams.
pub fn frame_time(n: u64) -> u64 {
    1_700_000_000_000_000 + n * 33_367
}

/// Bit writer for H.264 and H.265 syntax: u(n), ue(v), se(v).
pub struct Bits {
    out: Vec<u8>,
    cur: u8,
    n: u8,
}

impl Bits {
    pub fn new() -> Self {
        Bits { out: Vec::new(), cur: 0, n: 0 }
    }

    pub fn u(&mut self, bits: u32, value: u64) {
        for i in (0..bits).rev() {
            self.cur = self.cur << 1 | ((value >> i) & 1) as u8;
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.cur);
                self.cur = 0;
                self.n = 0;
            }
        }
    }

    pub fn ue(&mut self, value: u32) {
        let x = u64::from(value) + 1;
        let len = 64 - x.leading_zeros();
        self.u(len - 1, 0);
        self.u(len, x);
    }

    pub fn se(&mut self, value: i32) {
        self.ue(if value > 0 { 2 * value as u32 - 1 } else { 2 * value.unsigned_abs() });
    }

    fn aligned(&self) -> bool {
        self.n == 0
    }

    /// rbsp_trailing_bits (or byte_alignment in H.265 slices).
    pub fn finish(mut self) -> Vec<u8> {
        self.u(1, 1);
        while !self.aligned() {
            self.u(1, 0);
        }
        self.out
    }
}

/// A NAL unit in byte stream form, 4-byte start code, with emulation
/// prevention.
pub fn nal(header: &[u8], rbsp: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(header);
    let mut zeros = 0;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
}

/// The SEI RBSP of user data messages, in order.
fn sei_rbsp(messages: &[([u8; 16], Vec<u8>)]) -> Vec<u8> {
    let mut r = Vec::new();
    for (uuid, payload) in messages {
        r.push(5);
        let mut size = 16 + payload.len();
        while size >= 255 {
            r.push(0xFF);
            size -= 255;
        }
        r.push(size as u8);
        r.extend_from_slice(uuid);
        r.extend_from_slice(payload);
    }
    r.push(0x80);
    r
}

/// Frame type of decode-order frame `n`: 0 IDR, 1 P, 2 B. With B-frames
/// the order is I P B B P B B (`synth::shown`).
pub fn frame_type(n: u64, b_frames: bool) -> u8 {
    if n == 0 {
        0
    } else if !b_frames || n % 3 == 1 {
        1
    } else {
        2
    }
}

/// Presentation position of decode-order frame `n` (`synth::shown` with
/// B-frames).
fn shown(n: u64, b_frames: bool) -> u64 {
    if !b_frames || n == 0 {
        n
    } else if n % 3 == 1 {
        n + 2
    } else {
        n - 1
    }
}

/// Opaque slice data: `n` bytes ending in a non-zero byte, without three
/// zero-led bytes in a row.
fn slice_data(n: usize, seed: u64) -> Vec<u8> {
    let mut v: Vec<u8> = (0..n).map(|i| 0x40 | ((i as u64 * 7 + seed) % 61) as u8).collect();
    if let Some(last) = v.last_mut() {
        *last = 0x80;
    }
    v
}

/// The user data messages of an access unit's prefix, as the test expects
/// to read them, in order.
pub fn expected_user_data(cfg: &EsConfig, n: u64) -> Vec<([u8; 16], Vec<u8>)> {
    let mut v = Vec::new();
    if cfg.other {
        v.push((OTHER_UUID, other_payload(n)));
    }
    if cfg.misp {
        v.push((cfg.video.misp_uuid(), st0604_payload(0x9F, frame_time(n))));
    }
    v
}

fn other_payload(n: u64) -> Vec<u8> {
    vec![0x11, n as u8, 0x22]
}

/// Access unit `n` (decode order). With `len`, the slice data is sized so
/// the access unit has exactly that many bytes (when it can).
pub fn access_unit(cfg: &EsConfig, n: u64, b_frames: bool, len: Option<usize>) -> Vec<u8> {
    let head = prefix(cfg, n, b_frames);
    let mut au = head.clone();
    let slice = slice_nal(cfg, n, b_frames, cfg.slice_bytes);
    au.extend_from_slice(&slice);
    if let Some(len) = len {
        if len > au.len() {
            let extra = len - au.len();
            let mut au = head;
            au.extend_from_slice(&slice_nal(cfg, n, b_frames, cfg.slice_bytes + extra));
            return au;
        }
        if len < au.len() {
            let short = au.len() - len;
            let mut au = head;
            au.extend_from_slice(&slice_nal(cfg, n, b_frames, cfg.slice_bytes.saturating_sub(short).max(4)));
            return au;
        }
    }
    au
}

/// Everything before the first slice.
fn prefix(cfg: &EsConfig, n: u64, b_frames: bool) -> Vec<u8> {
    let t = frame_type(n, b_frames);
    let mut au = Vec::new();
    let mut messages = Vec::new();
    if cfg.other {
        messages.push((OTHER_UUID, other_payload(n)));
    }
    let misp = (cfg.video.misp_uuid(), st0604_payload(0x9F, frame_time(n)));
    match cfg.video {
        Video::H264 => {
            // AUD: primary_pic_type I, I/P or I/P/B.
            nal(&[0x09], &[(t << 5) | 0x10], &mut au);
            if t == 0 {
                h264_parameter_sets(&mut au);
            }
            if cfg.misp && cfg.misp_second {
                messages.push(misp.clone());
            }
            if !messages.is_empty() {
                nal(&[0x06], &sei_rbsp(&messages), &mut au);
            }
            if cfg.misp && !cfg.misp_second {
                nal(&[0x06], &sei_rbsp(&[misp]), &mut au);
            }
            if cfg.prefix_nal {
                // nal_unit_header_svc_extension (svc_extension_flag 1, base
                // layer), then prefix_nal_unit_svc(): store_ref_base_pic_flag
                // and additional_prefix_nal_unit_extension_flag 0 when
                // nal_ref_idc is not 0, then rbsp_trailing_bits.
                let (ref_idc, rbsp) = if t == 2 { (0, 0x80) } else { (0x60, 0x20) };
                nal(&[ref_idc | 14, 0x80, 0x80, 0x03], &[rbsp], &mut au);
            }
        }
        Video::H265 => {
            let tid1 = if t == 2 { 2 } else { 1 };
            nal(&[35 << 1, tid1], &[(t << 5) | 0x10], &mut au);
            if t == 0 {
                h265_parameter_sets(&mut au);
            }
            if cfg.misp && cfg.misp_second {
                messages.push(misp.clone());
            }
            if !messages.is_empty() {
                nal(&[39 << 1, tid1], &sei_rbsp(&messages), &mut au);
            }
            if cfg.misp && !cfg.misp_second {
                nal(&[39 << 1, tid1], &sei_rbsp(&[misp]), &mut au);
            }
        }
        Video::Mpeg2 => {
            if t == 0 {
                // sequence_header: 16x16, aspect 1, 29.97 Hz, bit rate,
                // marker, VBV, no matrices.
                au.extend_from_slice(&[0, 0, 1, 0xB3, 0x01, 0x00, 0x10, 0x14, 0xFF, 0xFF, 0xE0, 0x18]);
                au.extend_from_slice(&[0, 0, 1, 0xB8, 0x00, 0x08, 0x00, 0x00]);
            }
            // picture_header: temporal_reference, picture_coding_type.
            let coding = [1u8, 2, 3][usize::from(t)];
            au.extend_from_slice(&[0, 0, 1, 0x00, (shown(n, b_frames) >> 2) as u8, (coding << 3) | 0x07, 0xFF, 0xF8]);
            for (uuid, payload) in &messages {
                au.extend_from_slice(&[0, 0, 1, 0xB2]);
                au.extend_from_slice(uuid);
                au.extend_from_slice(payload);
            }
            if cfg.misp {
                au.extend_from_slice(&[0, 0, 1, 0xB2]);
                au.extend_from_slice(&misp.0);
                au.extend_from_slice(&misp.1);
            }
        }
    }
    au
}

fn h264_parameter_sets(au: &mut Vec<u8>) {
    let mut s = Bits::new();
    s.u(8, 77); // profile_idc: Main
    s.u(8, 0); // constraint flags, reserved
    s.u(8, 30); // level_idc
    s.ue(0); // seq_parameter_set_id
    s.ue(0); // log2_max_frame_num_minus4
    s.ue(0); // pic_order_cnt_type
    s.ue(4); // log2_max_pic_order_cnt_lsb_minus4
    s.ue(2); // max_num_ref_frames
    s.u(1, 0); // gaps_in_frame_num_value_allowed_flag
    s.ue(0); // pic_width_in_mbs_minus1
    s.ue(0); // pic_height_in_map_units_minus1
    s.u(1, 1); // frame_mbs_only_flag
    s.u(1, 1); // direct_8x8_inference_flag
    s.u(1, 0); // frame_cropping_flag
    s.u(1, 0); // vui_parameters_present_flag
    nal(&[0x67], &s.finish(), au);
    let mut p = Bits::new();
    p.ue(0); // pic_parameter_set_id
    p.ue(0); // seq_parameter_set_id
    p.u(1, 0); // entropy_coding_mode_flag
    p.u(1, 0); // bottom_field_pic_order_in_frame_present_flag
    p.ue(0); // num_slice_groups_minus1
    p.ue(0); // num_ref_idx_l0_default_active_minus1
    p.ue(0); // num_ref_idx_l1_default_active_minus1
    p.u(1, 0); // weighted_pred_flag
    p.u(2, 0); // weighted_bipred_idc
    p.se(0); // pic_init_qp_minus26
    p.se(0); // pic_init_qs_minus26
    p.se(0); // chroma_qp_index_offset
    p.u(1, 0); // deblocking_filter_control_present_flag
    p.u(1, 0); // constrained_intra_pred_flag
    p.u(1, 0); // redundant_pic_cnt_present_flag
    nal(&[0x68], &p.finish(), au);
}

/// profile_tier_level(1, 1): Main, level 1, two sub-layers without their
/// own profile or level.
fn h265_ptl(b: &mut Bits) {
    b.u(2, 0); // general_profile_space
    b.u(1, 0); // general_tier_flag
    b.u(5, 1); // general_profile_idc: Main
    b.u(32, 0x6000_0000); // general_profile_compatibility_flag[1], [2]
    b.u(1, 1); // general_progressive_source_flag
    b.u(1, 0); // general_interlaced_source_flag
    b.u(1, 0); // general_non_packed_constraint_flag
    b.u(1, 1); // general_frame_only_constraint_flag
    b.u(44, 0); // reserved zero bits, general_inbld_flag
    b.u(8, 30); // general_level_idc
    b.u(1, 0); // sub_layer_profile_present_flag[0]
    b.u(1, 0); // sub_layer_level_present_flag[0]
    b.u(14, 0); // reserved_zero_2bits[1..8]
}

fn h265_parameter_sets(au: &mut Vec<u8>) {
    let mut v = Bits::new();
    v.u(4, 0); // vps_video_parameter_set_id
    v.u(1, 1); // vps_base_layer_internal_flag
    v.u(1, 1); // vps_base_layer_available_flag
    v.u(6, 0); // vps_max_layers_minus1
    v.u(3, 1); // vps_max_sub_layers_minus1
    v.u(1, 1); // vps_temporal_id_nesting_flag
    v.u(16, 0xFFFF); // vps_reserved_0xffff_16bits
    h265_ptl(&mut v);
    v.u(1, 1); // vps_sub_layer_ordering_info_present_flag
    for _ in 0..2 {
        v.ue(2); // vps_max_dec_pic_buffering_minus1
        v.ue(1); // vps_max_num_reorder_pics
        v.ue(0); // vps_max_latency_increase_plus1
    }
    v.u(6, 0); // vps_max_layer_id
    v.ue(0); // vps_num_layer_sets_minus1
    v.u(1, 0); // vps_timing_info_present_flag
    v.u(1, 0); // vps_extension_flag
    nal(&[32 << 1, 1], &v.finish(), au);
    let mut s = Bits::new();
    s.u(4, 0); // sps_video_parameter_set_id
    s.u(3, 1); // sps_max_sub_layers_minus1
    s.u(1, 1); // sps_temporal_id_nesting_flag
    h265_ptl(&mut s);
    s.ue(0); // sps_seq_parameter_set_id
    s.ue(1); // chroma_format_idc
    s.ue(16); // pic_width_in_luma_samples
    s.ue(16); // pic_height_in_luma_samples
    s.u(1, 0); // conformance_window_flag
    s.ue(0); // bit_depth_luma_minus8
    s.ue(0); // bit_depth_chroma_minus8
    s.ue(4); // log2_max_pic_order_cnt_lsb_minus4
    s.u(1, 1); // sps_sub_layer_ordering_info_present_flag
    for _ in 0..2 {
        s.ue(2); // sps_max_dec_pic_buffering_minus1
        s.ue(1); // sps_max_num_reorder_pics
        s.ue(0); // sps_max_latency_increase_plus1
    }
    s.ue(0); // log2_min_luma_coding_block_size_minus3
    s.ue(1); // log2_diff_max_min_luma_coding_block_size
    s.ue(0); // log2_min_luma_transform_block_size_minus2
    s.ue(1); // log2_diff_max_min_luma_transform_block_size
    s.ue(0); // max_transform_hierarchy_depth_inter
    s.ue(0); // max_transform_hierarchy_depth_intra
    s.u(1, 0); // scaling_list_enabled_flag
    s.u(1, 0); // amp_enabled_flag
    s.u(1, 0); // sample_adaptive_offset_enabled_flag
    s.u(1, 0); // pcm_enabled_flag
    s.ue(0); // num_short_term_ref_pic_sets
    s.u(1, 0); // long_term_ref_pics_present_flag
    s.u(1, 0); // sps_temporal_mvp_enabled_flag
    s.u(1, 0); // strong_intra_smoothing_enabled_flag
    s.u(1, 0); // vui_parameters_present_flag
    s.u(1, 0); // sps_extension_present_flag
    nal(&[33 << 1, 1], &s.finish(), au);
    let mut p = Bits::new();
    p.ue(0); // pps_pic_parameter_set_id
    p.ue(0); // pps_seq_parameter_set_id
    p.u(1, 0); // dependent_slice_segments_enabled_flag
    p.u(1, 0); // output_flag_present_flag
    p.u(3, 0); // num_extra_slice_header_bits
    p.u(1, 0); // sign_data_hiding_enabled_flag
    p.u(1, 0); // cabac_init_present_flag
    p.ue(0); // num_ref_idx_l0_default_active_minus1
    p.ue(0); // num_ref_idx_l1_default_active_minus1
    p.se(0); // init_qp_minus26
    p.u(1, 0); // constrained_intra_pred_flag
    p.u(1, 0); // transform_skip_enabled_flag
    p.u(1, 0); // cu_qp_delta_enabled_flag
    p.se(0); // pps_cb_qp_offset
    p.se(0); // pps_cr_qp_offset
    p.u(1, 0); // pps_slice_chroma_qp_offsets_present_flag
    p.u(1, 0); // weighted_pred_flag
    p.u(1, 0); // weighted_bipred_flag
    p.u(1, 0); // transquant_bypass_enabled_flag
    p.u(1, 0); // tiles_enabled_flag
    p.u(1, 0); // entropy_coding_sync_enabled_flag
    p.u(1, 0); // pps_loop_filter_across_slices_enabled_flag
    p.u(1, 0); // deblocking_filter_control_present_flag
    p.u(1, 0); // pps_scaling_list_data_present_flag
    p.u(1, 0); // lists_modification_present_flag
    p.ue(0); // log2_parallel_merge_level_minus2
    p.u(1, 0); // slice_segment_header_extension_present_flag
    p.u(1, 0); // pps_extension_present_flag
    nal(&[34 << 1, 1], &p.finish(), au);
}

/// The slice NAL unit of access unit `n` with `data` slice data bytes.
fn slice_nal(cfg: &EsConfig, n: u64, b_frames: bool, data: usize) -> Vec<u8> {
    let t = frame_type(n, b_frames);
    let pos = shown(n, b_frames);
    let mut out = Vec::new();
    match cfg.video {
        Video::H264 => {
            // Reference pictures decoded before this one: frame_num.
            let refs = (0..n).filter(|&k| frame_type(k, b_frames) != 2).count() as u64;
            let mut b = Bits::new();
            b.ue(0); // first_mb_in_slice
            b.ue([7, 5, 6][usize::from(t)]); // slice_type: I, P, B (all slices)
            b.ue(0); // pic_parameter_set_id
            b.u(4, refs % 16); // frame_num
            if t == 0 {
                b.ue(0); // idr_pic_id
            }
            b.u(8, (2 * pos) % 256); // pic_order_cnt_lsb
            if t == 2 {
                b.u(1, 1); // direct_spatial_mv_pred_flag
            }
            if t != 0 {
                b.u(1, 0); // num_ref_idx_active_override_flag
                b.u(1, 0); // ref_pic_list_modification_flag_l0
            }
            if t == 2 {
                b.u(1, 0); // ref_pic_list_modification_flag_l1
            }
            match t {
                0 => {
                    b.u(1, 0); // no_output_of_prior_pics_flag
                    b.u(1, 0); // long_term_reference_flag
                }
                1 => b.u(1, 0), // adaptive_ref_pic_marking_mode_flag
                _ => {}         // nal_ref_idc 0: no dec_ref_pic_marking
            }
            b.se(0); // slice_qp_delta
            let mut rbsp = b.finish();
            rbsp.extend(slice_data(data, n));
            let header = [0x65, 0x41, 0x01][usize::from(t)];
            nal(&[header], &rbsp, &mut out);
        }
        Video::H265 => {
            let mut b = Bits::new();
            b.u(1, 1); // first_slice_segment_in_pic_flag
            if t == 0 {
                b.u(1, 0); // no_output_of_prior_pics_flag
            }
            b.ue(0); // slice_pic_parameter_set_id
            b.ue([2, 1, 0][usize::from(t)]); // slice_type: I, P, B
            if t != 0 {
                b.u(8, pos % 256); // slice_pic_order_cnt_lsb
                b.u(1, 0); // short_term_ref_pic_set_sps_flag
                // st_ref_pic_set(0): the anchors this picture uses.
                let (neg, posd) = match (t, b_frames, n % 3) {
                    (1, true, _) => (3, None),
                    (1, false, _) => (1, None),
                    (_, _, 2) => (1, Some(2)),
                    _ => (2, Some(1)),
                };
                b.ue(1); // num_negative_pics
                b.ue(u32::from(posd.is_some())); // num_positive_pics
                b.ue(neg - 1); // delta_poc_s0_minus1
                b.u(1, 1); // used_by_curr_pic_s0_flag
                if let Some(d) = posd {
                    b.ue(d - 1); // delta_poc_s1_minus1
                    b.u(1, 1); // used_by_curr_pic_s1_flag
                }
                b.u(1, 0); // num_ref_idx_active_override_flag
                if t == 2 {
                    b.u(1, 0); // mvd_l1_zero_flag
                }
                b.ue(0); // five_minus_max_num_merge_cand
            }
            b.se(0); // slice_qp_delta
            let mut rbsp = b.finish(); // byte_alignment()
            rbsp.extend(slice_data(data, n));
            // IDR_W_RADL, TRAIL_R (TemporalId 0), TRAIL_N (TemporalId 1).
            let header: [u8; 2] = match t {
                0 => [19 << 1, 1],
                1 => [1 << 1, 1],
                _ => [0, 2],
            };
            nal(&header, &rbsp, &mut out);
        }
        Video::Mpeg2 => {
            out.extend_from_slice(&[0, 0, 1, 0x01, 0x0A]);
            out.extend(slice_data(data, n));
        }
    }
    out
}
