use crate::encode::block_store::{SharedEncodedBlockStore, StoredCodeBlock, StoredTier1Layout};
use crate::encode::profile_enter;
use crate::error::{Jp2LamError, Result};
use crate::plan::ProgressionOrder;
use crate::t2::{PacketSequenceBuilder, StoredPacket, StoredPayloadRange, TilePartPayload};

use super::t1::{NativeEncodedTier1Band, NativeEncodedTier1Layout, NativeTier1SelectionLayout};

/// Part of the legacy pipeline superseded by `TilePartPayload`-direct
/// building; kept under `#[cfg(test)]` — see `native::layout`'s module doc
/// comment.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativePacket {
    pub resolution: u8,
    pub codeblock_count: usize,
    pub pass_count: usize,
    pub header: Vec<u8>,
    pub body: Vec<u8>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativePacketSequence {
    pub packets: Vec<NativePacket>,
}

// ---------------------------------------------------------------------------
// BIO — big-endian bit writer for packet headers (ISO 15444-1 §B.2)
// ---------------------------------------------------------------------------

struct PacketBio {
    buf: Vec<u8>,
    reg: u32,
    ct: u32,
}

impl PacketBio {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            reg: 0,
            ct: 8,
        }
    }

    fn byteout(&mut self) {
        self.reg = (self.reg << 8) & 0xffff;
        self.ct = if self.reg == 0xff00 { 7 } else { 8 };
        self.buf.push((self.reg >> 8) as u8);
    }

    fn putbit(&mut self, b: u32) {
        if self.ct == 0 {
            self.byteout();
        }
        self.ct -= 1;
        self.reg |= b << self.ct;
    }

    fn write_bits(&mut self, v: u32, n: u32) {
        for i in (0..n as i32).rev() {
            self.putbit((v >> i) & 1);
        }
    }

    fn flush(mut self) -> Vec<u8> {
        self.byteout();
        if self.ct == 7 {
            self.byteout();
        }
        self.buf
    }
}

// ---------------------------------------------------------------------------
// Tag tree — hierarchical min-value coder (ISO 15444-1 §B.10.2)
// ---------------------------------------------------------------------------

const TGT_NO_PARENT: u32 = u32::MAX;

#[derive(Clone)]
struct TgtNode {
    parent: u32,
    value: i32,
    low: i32,
    known: u8,
}

struct TagTree {
    nodes: Vec<TgtNode>,
}

impl TagTree {
    fn new(numleafsh: usize, numleafsv: usize) -> Self {
        let mut nplh = [0i32; 32];
        let mut nplv = [0i32; 32];
        let mut numlvls = 0usize;
        let mut numnodes = 0usize;
        nplh[0] = numleafsh as i32;
        nplv[0] = numleafsv as i32;
        loop {
            let n = (nplh[numlvls] * nplv[numlvls]) as usize;
            nplh[numlvls + 1] = (nplh[numlvls] + 1) / 2;
            nplv[numlvls + 1] = (nplv[numlvls] + 1) / 2;
            numnodes += n;
            numlvls += 1;
            if n <= 1 {
                break;
            }
        }
        let default_node = TgtNode {
            parent: TGT_NO_PARENT,
            value: 999,
            low: 0,
            known: 0,
        };
        let mut nodes = vec![default_node; numnodes];
        Self::link_parents(&mut nodes, numleafsh, numleafsv, numlvls, &nplh, &nplv);
        Self { nodes }
    }

    fn link_parents(
        nodes: &mut [TgtNode],
        numleafsh: usize,
        numleafsv: usize,
        numlvls: usize,
        nplh: &[i32; 32],
        nplv: &[i32; 32],
    ) {
        let mut node_idx = 0usize;
        let mut l_parent_idx = numleafsh * numleafsv;
        let mut l_parent_idx0 = l_parent_idx;
        for i in 0..numlvls.saturating_sub(1) {
            let mut j = 0i32;
            while j < nplv[i] {
                let mut k = nplh[i];
                loop {
                    k -= 1;
                    if k < 0 {
                        break;
                    }
                    nodes[node_idx].parent = l_parent_idx as u32;
                    node_idx += 1;
                    k -= 1;
                    if k >= 0 {
                        nodes[node_idx].parent = l_parent_idx as u32;
                        node_idx += 1;
                    }
                    l_parent_idx += 1;
                }
                if j & 1 != 0 || j == nplv[i] - 1 {
                    l_parent_idx0 = l_parent_idx;
                } else {
                    l_parent_idx = l_parent_idx0;
                    l_parent_idx0 = l_parent_idx0 + nplh[i] as usize;
                }
                j += 1;
            }
        }
        if node_idx < nodes.len() {
            nodes[node_idx].parent = TGT_NO_PARENT;
        }
    }

    fn reset(&mut self) {
        for node in &mut self.nodes {
            node.value = 999;
            node.low = 0;
            node.known = 0;
        }
    }

    fn set_value(&mut self, leafno: usize, value: i32) {
        let mut idx = leafno;
        loop {
            let node = &mut self.nodes[idx];
            if node.value <= value {
                break;
            }
            node.value = value;
            if node.parent == TGT_NO_PARENT {
                break;
            }
            idx = node.parent as usize;
        }
    }

    fn encode(&mut self, bio: &mut PacketBio, leafno: usize, threshold: i32) {
        let mut stk = [0usize; 32];
        let mut depth = 0usize;
        let mut node_idx = leafno;
        while self.nodes[node_idx].parent != TGT_NO_PARENT {
            stk[depth] = node_idx;
            depth += 1;
            node_idx = self.nodes[node_idx].parent as usize;
        }
        let mut low = 0i32;
        loop {
            {
                let node = &mut self.nodes[node_idx];
                if low > node.low {
                    node.low = low;
                } else {
                    low = node.low;
                }
                while low < threshold {
                    if low >= node.value {
                        if node.known == 0 {
                            bio.putbit(1);
                            node.known = 1;
                        }
                        break;
                    } else {
                        bio.putbit(0);
                        low += 1;
                    }
                }
                node.low = low;
            }
            if depth == 0 {
                break;
            }
            depth -= 1;
            node_idx = stk[depth];
        }
    }
}

// ---------------------------------------------------------------------------
// Pass-count and comma-code helpers (ISO 15444-1 §B.10.6)
// ---------------------------------------------------------------------------

fn floor_log2(v: u32) -> u32 {
    if v == 0 { 0 } else { 31 - v.leading_zeros() }
}

fn numpasses_codeword(numpasses: u32) -> (u32, u32) {
    // ISO/IEC 15444-1 Annex B.10.6, Table B-4.
    if numpasses == 1 {
        (0, 1)
    } else if numpasses == 2 {
        (0b10, 2)
    } else if numpasses <= 5 {
        (0xc | (numpasses - 3), 4)
    } else if numpasses <= 36 {
        (0x1e0 | (numpasses - 6), 9)
    } else if numpasses <= 164 {
        (0xff80 | (numpasses - 37), 16)
    } else {
        // Clamp at maximum supported pass count.
        (0xff80 | (164 - 37), 16)
    }
}

fn put_numpasses(bio: &mut PacketBio, numpasses: u32) {
    let (codeword, width) = numpasses_codeword(numpasses);
    bio.write_bits(codeword, width);
}

fn put_commacode(bio: &mut PacketBio, n: u32) {
    for _ in 0..n {
        bio.putbit(1);
    }
    bio.putbit(0);
}

fn codeblock_segments(passes: &[super::t1::NativeEncodedTier1Pass]) -> Vec<(u32, u32)> {
    let mut segments = Vec::new();
    let mut seg_len = 0u32;
    let mut seg_nump = 0u32;

    if passes.is_empty() {
        return segments;
    }

    let last = passes.len() - 1;
    for (passno, pass) in passes.iter().enumerate() {
        seg_nump += 1;
        seg_len += pass.length as u32;
        let boundary = pass.is_terminated || passno == last;
        if boundary {
            segments.push((seg_len, seg_nump));
            seg_len = 0;
            seg_nump = 0;
        }
    }

    segments
}

fn length_increment(numlenbits: u32, segments: &[(u32, u32)]) -> u32 {
    segments.iter().fold(0, |increment, &(seg_len, seg_nump)| {
        if seg_len == 0 {
            return increment;
        }
        let needed = floor_log2(seg_len) + 1;
        let have = numlenbits + floor_log2(seg_nump);
        increment.max(needed.saturating_sub(have))
    })
}

fn selected_pass_count(
    block: &super::t1::NativeEncodedTier1CodeBlock,
    selected_band: Option<&super::t1::NativeTier1SelectionBand>,
    block_index: usize,
) -> usize {
    selected_band
        .and_then(|band| band.selected_passes.get(block_index).copied())
        .map(usize::from)
        .unwrap_or(block.passes.len())
        .min(block.passes.len())
}

// ---------------------------------------------------------------------------
// Per-band codeblock count helpers
// ---------------------------------------------------------------------------

fn band_cb_dims(band: &NativeEncodedTier1Band) -> (usize, usize) {
    if band.blocks.is_empty() {
        return (0, 0);
    }
    let x0 = band.blocks.iter().map(|b| b.x0).min().unwrap();
    let y0 = band.blocks.iter().map(|b| b.y0).min().unwrap();
    let cbw = band.blocks[0].x1 - band.blocks[0].x0;
    let cbh = band.blocks[0].y1 - band.blocks[0].y0;
    if cbw == 0 || cbh == 0 {
        return (1, band.blocks.len());
    }
    let x1 = band.blocks.iter().map(|b| b.x1).max().unwrap();
    let y1 = band.blocks.iter().map(|b| b.y1).max().unwrap();
    let cx = (x1 - x0).div_ceil(cbw);
    let cy = (y1 - y0).div_ceil(cbh);
    (cx, cy)
}

// ---------------------------------------------------------------------------
// Real ISO 15444-1 packet header encoder
// ---------------------------------------------------------------------------

/// Build one full LRCP packet header for a single (layer, resolution,
/// component, precinct). `bands` are listed in codestream order (LL for the
/// lowest resolution; HL/LH/HH for detail resolutions).
fn build_packet_header(
    bands: &[&NativeEncodedTier1Band],
    selected_bands: Option<&[&super::t1::NativeTier1SelectionBand]>,
) -> Vec<u8> {
    let mut bio = PacketBio::new();
    let packet_has_data = bands.iter().enumerate().any(|(band_index, band)| {
        let selected_band = selected_bands.and_then(|bands| bands.get(band_index).copied());
        band.blocks
            .iter()
            .enumerate()
            .any(|(block_index, block)| selected_pass_count(block, selected_band, block_index) > 0)
    });
    if !packet_has_data {
        bio.putbit(0);
        let header = bio.flush();
        crate::encode::counters::record_packet_header(header.len());
        return header;
    }

    bio.putbit(1);

    for (band_index, band) in bands.iter().enumerate() {
        let selected_band = selected_bands.and_then(|bands| bands.get(band_index).copied());
        let n = band.blocks.len();
        if n == 0 {
            continue;
        }

        let (cx, cy) = band_cb_dims(band);
        let mut incltree = TagTree::new(cx.max(1), cy.max(1));
        let mut imsbtree = TagTree::new(cx.max(1), cy.max(1));
        incltree.reset();
        imsbtree.reset();

        for (cblkno, cblk) in band.blocks.iter().enumerate() {
            imsbtree.set_value(cblkno, cblk.zero_bitplanes as i32);
            if selected_pass_count(cblk, selected_band, cblkno) > 0 {
                incltree.set_value(cblkno, 0);
            }
        }

        let mut numlenbits_per_block: Vec<u32> = vec![3; n];

        for (cblkno, cblk) in band.blocks.iter().enumerate() {
            incltree.encode(&mut bio, cblkno, 1);

            let selected_count = selected_pass_count(cblk, selected_band, cblkno);
            if selected_count == 0 {
                continue;
            }

            imsbtree.encode(&mut bio, cblkno, 999);

            let selected_passes = &cblk.passes[..selected_count];
            let numpasses = selected_passes.len() as u32;
            put_numpasses(&mut bio, numpasses);

            let numlenbits = numlenbits_per_block[cblkno];

            // ISO/IEC 15444-1 Annex B.10.7 applies one Lblock increment at the
            // start of the sequence, sized for the largest segment in the packet.
            let segments = codeblock_segments(selected_passes);
            let increment = length_increment(numlenbits, &segments);

            put_commacode(&mut bio, increment);
            numlenbits_per_block[cblkno] = numlenbits + increment;

            let numlenbits_final = numlenbits_per_block[cblkno];
            for (seg_len, seg_nump) in segments {
                let bits = numlenbits_final + floor_log2(seg_nump);
                bio.write_bits(seg_len, bits);
            }
        }
    }

    let header = bio.flush();
    crate::encode::counters::record_packet_header(header.len());
    header
}

// ---------------------------------------------------------------------------
// Packet body: concatenated pass bytes per codeblock
// ---------------------------------------------------------------------------

#[cfg(test)]
fn build_packet_body(
    band: &NativeEncodedTier1Band,
    selected_band: Option<&super::t1::NativeTier1SelectionBand>,
) -> Vec<u8> {
    let mut body = Vec::new();
    for (cblkno, cblk) in band.blocks.iter().enumerate() {
        let selected_count = selected_pass_count(cblk, selected_band, cblkno);
        if selected_count > 0 {
            let last_pass = &cblk.passes[selected_count - 1];
            let selected_len = last_pass.cumulative_length.min(cblk.payload.len());
            body.extend_from_slice(&cblk.payload[..selected_len]);
        }
    }
    body
}

// ---------------------------------------------------------------------------
// Resolution-level packet grouping
// ---------------------------------------------------------------------------

/// `(layer, resolution, component)` triples in the order one tile's packets
/// are written (Annex B.12).
///
/// The encoder emits one precinct per subband, so the position loop of
/// B.12.1.3-B.12.1.5 visits a single position (the tile origin, where every
/// resolution's only precinct starts). What remains is the nesting of layer,
/// resolution and component: LRCP, RLCP and RPCL are resolution-major over
/// component and differ in where the layer loop sits; PCRL and CPRL are
/// component-major with the layer loop innermost.
pub(crate) fn packet_order(
    order: ProgressionOrder,
    max_resolution: u8,
    component_count: usize,
    layers: u16,
) -> Vec<(u16, u8, usize)> {
    let resolutions = 0..=max_resolution;
    let components = 0..component_count;
    let layers = 0..layers;
    let mut packets =
        Vec::with_capacity((usize::from(max_resolution) + 1) * component_count * layers.len());
    match order {
        ProgressionOrder::Lrcp => {
            for l in layers {
                for r in resolutions.clone() {
                    for c in components.clone() {
                        packets.push((l, r, c));
                    }
                }
            }
        }
        ProgressionOrder::Rlcp => {
            for r in resolutions {
                for l in layers.clone() {
                    for c in components.clone() {
                        packets.push((l, r, c));
                    }
                }
            }
        }
        ProgressionOrder::Rpcl => {
            for r in resolutions {
                for c in components.clone() {
                    for l in layers.clone() {
                        packets.push((l, r, c));
                    }
                }
            }
        }
        ProgressionOrder::Pcrl | ProgressionOrder::Cprl => {
            for c in components {
                for r in resolutions.clone() {
                    for l in layers.clone() {
                        packets.push((l, r, c));
                    }
                }
            }
        }
    }
    packets
}

// ---------------------------------------------------------------------------
// Layered packet headers (ISO 15444-1 B.10, state carried across layers)
// ---------------------------------------------------------------------------

/// What a packet header needs to know about one code-block.
#[derive(Clone, Copy)]
struct HeaderBlock<'a> {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    zero_bitplanes: u8,
    passes: &'a [super::t1::NativeEncodedTier1Pass],
}

impl<'a> From<&'a StoredCodeBlock> for HeaderBlock<'a> {
    fn from(block: &'a StoredCodeBlock) -> Self {
        Self {
            x0: block.x0,
            y0: block.y0,
            x1: block.x1,
            y1: block.y1,
            zero_bitplanes: block.zero_bitplanes,
            passes: &block.passes,
        }
    }
}

fn header_cb_dims(blocks: &[HeaderBlock<'_>]) -> (usize, usize) {
    let Some(first) = blocks.first() else {
        return (0, 0);
    };
    let x0 = blocks.iter().map(|b| b.x0).min().unwrap_or(0);
    let y0 = blocks.iter().map(|b| b.y0).min().unwrap_or(0);
    let cbw = first.x1 - first.x0;
    let cbh = first.y1 - first.y0;
    if cbw == 0 || cbh == 0 {
        return (1, blocks.len());
    }
    let x1 = blocks.iter().map(|b| b.x1).max().unwrap_or(0);
    let y1 = blocks.iter().map(|b| b.y1).max().unwrap_or(0);
    ((x1 - x0).div_ceil(cbw), (y1 - y0).div_ceil(cbh))
}

/// Per-code-block header state that persists from one layer's packet to the
/// next for the same (resolution, component, precinct): whether the block has
/// been included yet (B.10.4) and its current `Lblock` (B.10.7.1).
struct BlockHeaderState {
    included: bool,
    lblock: u32,
}

struct BandHeaderState {
    inclusion: TagTree,
    zero_bitplanes: TagTree,
    blocks: Vec<BlockHeaderState>,
}

/// Header state for every band of one (resolution, component) packet, in the
/// band order the packets use.
pub(crate) struct PacketHeaderState {
    bands: Vec<BandHeaderState>,
}

impl PacketHeaderState {
    fn new(bands: &[Vec<HeaderBlock<'_>>]) -> Self {
        Self {
            bands: bands
                .iter()
                .map(|blocks| {
                    let (cx, cy) = header_cb_dims(blocks);
                    let mut inclusion = TagTree::new(cx.max(1), cy.max(1));
                    let mut zero_bitplanes = TagTree::new(cx.max(1), cy.max(1));
                    inclusion.reset();
                    zero_bitplanes.reset();
                    for (index, block) in blocks.iter().enumerate() {
                        zero_bitplanes.set_value(index, i32::from(block.zero_bitplanes));
                    }
                    BandHeaderState {
                        inclusion,
                        zero_bitplanes,
                        blocks: blocks
                            .iter()
                            .map(|_| BlockHeaderState {
                                included: false,
                                lblock: 3,
                            })
                            .collect(),
                    }
                })
                .collect(),
        }
    }
}

/// One layer's packet header for a (resolution, component) packet. `bands`
/// lists each band's blocks; `starts[b][i]..ends[b][i]` are the passes block
/// `i` of band `b` contributes to this layer (cumulative counts, so `starts`
/// is the previous layer's `ends`). `state` carries inclusion and `Lblock`
/// across layers and must be the same object for every layer of this packet.
fn build_layer_packet_header(
    bands: &[Vec<HeaderBlock<'_>>],
    starts: &[&[u16]],
    ends: &[&[u16]],
    layer: u16,
    state: &mut PacketHeaderState,
) -> Vec<u8> {
    let mut bio = PacketBio::new();
    let packet_has_data = starts
        .iter()
        .zip(ends)
        .any(|(start, end)| start.iter().zip(end.iter()).any(|(s, e)| e > s));
    if !packet_has_data {
        bio.putbit(0);
        let header = bio.flush();
        crate::encode::counters::record_packet_header(header.len());
        return header;
    }

    bio.putbit(1);
    for (band_index, blocks) in bands.iter().enumerate() {
        if blocks.is_empty() {
            continue;
        }
        let band_state = &mut state.bands[band_index];
        let pass_range = |index: usize, block: &HeaderBlock<'_>| {
            let start = usize::from(starts[band_index][index]).min(block.passes.len());
            let end = usize::from(ends[band_index][index])
                .min(block.passes.len())
                .max(start);
            (start, end)
        };

        // Every block first included in this layer must be in the inclusion
        // tag tree before any block of the band is coded: a parent node's
        // value is the minimum over its subtree, and the first leaf coded
        // under it commits that value to the stream (B.10.2).
        for (index, block) in blocks.iter().enumerate() {
            let (start, end) = pass_range(index, block);
            if !band_state.blocks[index].included && end > start {
                band_state.inclusion.set_value(index, i32::from(layer));
            }
        }

        for (index, block) in blocks.iter().enumerate() {
            let (start, end) = pass_range(index, block);
            let new_passes = end - start;
            let block_state = &mut band_state.blocks[index];

            // B.10.4: first inclusion is tag-tree coded against the layer
            // threshold; later inclusion is a single bit.
            if block_state.included {
                bio.putbit(u32::from(new_passes > 0));
            } else {
                band_state
                    .inclusion
                    .encode(&mut bio, index, i32::from(layer) + 1);
            }
            if new_passes == 0 {
                continue;
            }
            if !block_state.included {
                // B.10.5: zero bit-planes, only with the first inclusion.
                band_state.zero_bitplanes.encode(&mut bio, index, 999);
                block_state.included = true;
            }

            put_numpasses(&mut bio, new_passes as u32);

            // B.10.7: one Lblock increment for the layer, sized for the
            // largest codeword segment it carries, then one length per segment.
            let segments = codeblock_segments(&block.passes[start..end]);
            let increment = length_increment(block_state.lblock, &segments);
            put_commacode(&mut bio, increment);
            block_state.lblock += increment;
            for (seg_len, seg_nump) in segments {
                bio.write_bits(seg_len, block_state.lblock + floor_log2(seg_nump));
            }
        }
    }

    let header = bio.flush();
    crate::encode::counters::record_packet_header(header.len());
    header
}

/// The tile-0 convenience wrappers that used to sit here
/// (`build_packet_sequence`, `_for_components`, `_for_components_with_selections`)
/// had no caller anywhere, including tests, and were deleted. This function
/// itself is `#[cfg(test)]`-gated because it's part of the legacy pipeline
/// (see `native::layout`'s module doc comment) but does have direct test
/// coverage below.
#[cfg(test)]
pub(crate) fn build_packet_sequence_for_tile_components_with_selections(
    tile_index: u16,
    layouts: &[NativeEncodedTier1Layout],
    selections: Option<&[NativeTier1SelectionLayout]>,
) -> Result<NativePacketSequence> {
    let _p = profile_enter("build_packet_sequence_for_components");
    validate_layouts_for_tile(tile_index, layouts)?;
    if let Some(selections) = selections {
        if selections.len() != layouts.len() {
            return Err(Jp2LamError::EncodeFailed(format!(
                "component selection count {} does not match layout count {}",
                selections.len(),
                layouts.len()
            )));
        }
    }
    let max_resolution = layouts
        .iter()
        .flat_map(|component| component.bands.iter().map(|band| band.resolution))
        .max()
        .unwrap_or(0);

    let mut packets = Vec::new();

    for resolution in 0..=max_resolution {
        // LRCP ordering with one tile and one precinct still emits packets per
        // component at each resolution (L=0 fixed): (R,C).
        for (component_index, component) in layouts.iter().enumerate() {
            let bands: Vec<&NativeEncodedTier1Band> = component
                .bands
                .iter()
                .filter(|b| b.resolution == resolution)
                .collect();
            let selected_bands: Option<Vec<_>> = selections
                .and_then(|all| all.get(component_index))
                .map(|selection| {
                    selection
                        .bands
                        .iter()
                        .filter(|b| b.resolution == resolution)
                        .collect::<Vec<_>>()
                });

            if bands.is_empty() {
                continue;
            }

            let header = build_packet_header(&bands, selected_bands.as_deref());
            let mut body = Vec::new();
            let mut codeblock_count = 0usize;
            let mut pass_count = 0usize;
            for (band_index, band) in bands.iter().enumerate() {
                let selected_band = selected_bands
                    .as_ref()
                    .and_then(|bands| bands.get(band_index).copied());
                body.extend_from_slice(&build_packet_body(band, selected_band));
                codeblock_count += band.blocks.len();
                pass_count += band
                    .blocks
                    .iter()
                    .enumerate()
                    .map(|(block_index, block)| {
                        selected_pass_count(block, selected_band, block_index)
                    })
                    .sum::<usize>();
            }

            packets.push(NativePacket {
                resolution,
                codeblock_count,
                pass_count,
                header,
                body,
            });
        }
    }

    Ok(NativePacketSequence { packets })
}

/// Single-component convenience wrapper for tile 0. Only exercised by
/// tests; production always encodes multiple components together via
/// [`build_tile_part_payload_for_tile_components_owned`].
#[cfg(test)]
pub(crate) fn build_tile_part_payload(
    layout: &NativeEncodedTier1Layout,
) -> Result<TilePartPayload> {
    build_tile_part_payload_for_components(std::slice::from_ref(layout))
}

/// Tile-0, no-selections convenience wrapper. Only exercised by tests.
#[cfg(test)]
pub(crate) fn build_tile_part_payload_for_components(
    layouts: &[NativeEncodedTier1Layout],
) -> Result<TilePartPayload> {
    build_tile_part_payload_for_components_with_selections(layouts, None)
}

/// Tile-0 convenience wrapper over the borrowed tile-components form. Only
/// exercised by tests; production goes through the owned form via
/// [`build_tile_part_payload_for_tile_components_owned`].
#[cfg(test)]
pub(crate) fn build_tile_part_payload_for_components_with_selections(
    layouts: &[NativeEncodedTier1Layout],
    selections: Option<&[NativeTier1SelectionLayout]>,
) -> Result<TilePartPayload> {
    build_tile_part_payload_for_tile_components_with_selections(0, layouts, selections)
}

/// Borrowed-layouts form for an explicit tile index. Only exercised by
/// tests directly; production uses the owned form (which takes ownership of
/// the encoded layouts rather than borrowing them).
#[cfg(test)]
pub(crate) fn build_tile_part_payload_for_tile_components_with_selections(
    tile_index: u16,
    layouts: &[NativeEncodedTier1Layout],
    selections: Option<&[NativeTier1SelectionLayout]>,
) -> Result<TilePartPayload> {
    let _p = profile_enter("t2::build_tile_part_payload_for_components");
    let packet_sequence =
        build_packet_sequence_for_tile_components_with_selections(tile_index, layouts, selections)?;
    let mut builder = PacketSequenceBuilder::new();

    for packet in packet_sequence.packets {
        builder = builder.push_header_body_packet(packet.header, packet.body);
    }

    Ok(builder.finish_payload())
}

pub(crate) fn build_tile_part_payload_for_tile_components_owned(
    tile_index: u16,
    mut layouts: Vec<NativeEncodedTier1Layout>,
    selections: Option<&[NativeTier1SelectionLayout]>,
    order: ProgressionOrder,
) -> Result<TilePartPayload> {
    let _p = profile_enter("t2::build_tile_part_payload_for_components_owned");
    validate_layouts_for_tile(tile_index, &layouts)?;
    if let Some(selections) = selections {
        if selections.len() != layouts.len() {
            return Err(Jp2LamError::EncodeFailed(format!(
                "component selection count {} does not match layout count {}",
                selections.len(),
                layouts.len()
            )));
        }
    }

    let max_resolution = layouts
        .iter()
        .flat_map(|component| component.bands.iter().map(|band| band.resolution))
        .max()
        .unwrap_or(0);

    let mut builder = PacketSequenceBuilder::new();
    for (_, resolution, component_index) in packet_order(order, max_resolution, layouts.len(), 1) {
        {
            let component = &mut layouts[component_index];
            let band_indices = component
                .bands
                .iter()
                .enumerate()
                .filter_map(|(index, band)| (band.resolution == resolution).then_some(index))
                .collect::<Vec<_>>();
            if band_indices.is_empty() {
                continue;
            }

            let selected_band_indices =
                selections
                    .and_then(|all| all.get(component_index))
                    .map(|selection| {
                        selection
                            .bands
                            .iter()
                            .enumerate()
                            .filter_map(|(index, band)| {
                                (band.resolution == resolution).then_some(index)
                            })
                            .collect::<Vec<_>>()
                    });

            let header = {
                let bands = band_indices
                    .iter()
                    .map(|&index| &component.bands[index])
                    .collect::<Vec<_>>();
                let selected_bands = selected_band_indices.as_ref().and_then(|indices| {
                    selections
                        .and_then(|all| all.get(component_index))
                        .map(|selection| {
                            indices
                                .iter()
                                .map(|&index| &selection.bands[index])
                                .collect::<Vec<_>>()
                        })
                });
                build_packet_header(&bands, selected_bands.as_deref())
            };

            let mut body_segments = Vec::new();
            for (packet_band_index, &band_index) in band_indices.iter().enumerate() {
                let selected_band = selected_band_indices
                    .as_ref()
                    .and_then(|indices| indices.get(packet_band_index).copied())
                    .and_then(|selection_band_index| {
                        selections
                            .and_then(|all| all.get(component_index))
                            .and_then(|selection| selection.bands.get(selection_band_index))
                    });
                body_segments.extend(build_packet_body_segments(
                    &mut component.bands[band_index],
                    selected_band,
                ));
            }

            builder = builder.push_header_body_segments(header, body_segments);
        }
    }

    Ok(builder.finish_payload())
}

/// Build packet headers plus store-backed payload ranges for one tile, for
/// every quality layer.
///
/// `layers[l][c]` is the cumulative pass selection of component `c` after
/// layer `l`; layer `l` carries the passes between `layers[l-1]` and
/// `layers[l]`. Packet planning clones only compact Tier-1 metadata:
/// compressed bytes remain in `EncodedBlockStore` and are read directly when
/// the final codestream writer emits each selected range.
pub(crate) fn build_stored_tile_part_payload(
    tile_index: u16,
    store: SharedEncodedBlockStore,
    layouts: &[StoredTier1Layout],
    layers: &[Vec<NativeTier1SelectionLayout>],
    order: ProgressionOrder,
) -> Result<TilePartPayload> {
    if layouts.iter().any(|layout| layout.tile_index != tile_index) {
        return Err(Jp2LamError::EncodeFailed(format!(
            "stored Tier-1 layouts do not all belong to tile {tile_index}"
        )));
    }
    if layers.is_empty() {
        return Err(Jp2LamError::EncodeFailed(
            "a tile needs at least one quality layer".into(),
        ));
    }
    if layers.iter().any(|layer| layer.len() != layouts.len()) {
        return Err(Jp2LamError::EncodeFailed(format!(
            "stored component selection count does not match layout count {}",
            layouts.len()
        )));
    }
    for (layout, selection) in layouts.iter().zip(&layers[0]) {
        if selection.bands.len() != layout.bands.len()
            || selection
                .bands
                .iter()
                .zip(&layout.bands)
                .any(|(s, b)| s.selected_passes.len() != b.blocks.len())
        {
            return Err(Jp2LamError::EncodeFailed(
                "stored packet selection does not match the layout's band or block counts".into(),
            ));
        }
    }
    let max_resolution = layouts
        .iter()
        .flat_map(|layout| layout.bands.iter().map(|band| band.resolution))
        .max()
        .unwrap_or(0);
    let layer_count = u16::try_from(layers.len())
        .map_err(|_| Jp2LamError::EncodeFailed("more than 65535 quality layers".into()))?;

    // Per-(component, resolution) header state, created at that packet's
    // first layer and reused by its later ones.
    let resolutions = usize::from(max_resolution) + 1;
    let mut states: Vec<Option<PacketHeaderState>> =
        (0..layouts.len() * resolutions).map(|_| None).collect();
    let empty: Vec<u16> = Vec::new();
    let mut packets = Vec::new();

    for (layer, resolution, component_index) in
        packet_order(order, max_resolution, layouts.len(), layer_count)
    {
        let layout = &layouts[component_index];
        let band_indices = layout
            .bands
            .iter()
            .enumerate()
            .filter_map(|(index, band)| (band.resolution == resolution).then_some(index))
            .collect::<Vec<_>>();
        if band_indices.is_empty() {
            continue;
        }
        let bands: Vec<Vec<HeaderBlock<'_>>> = band_indices
            .iter()
            .map(|&index| {
                layout.bands[index]
                    .blocks
                    .iter()
                    .map(HeaderBlock::from)
                    .collect()
            })
            .collect();
        let ends: Vec<&[u16]> = band_indices
            .iter()
            .map(|&index| {
                layers[usize::from(layer)][component_index].bands[index]
                    .selected_passes
                    .as_slice()
            })
            .collect();
        let starts: Vec<&[u16]> = band_indices
            .iter()
            .map(|&index| match layer.checked_sub(1) {
                Some(previous) => layers[usize::from(previous)][component_index].bands[index]
                    .selected_passes
                    .as_slice(),
                None => empty.as_slice(),
            })
            .collect();
        // Layer 0 starts every block at pass 0.
        let zero_starts: Vec<Vec<u16>>;
        let starts: Vec<&[u16]> = if layer == 0 {
            zero_starts = ends.iter().map(|end| vec![0u16; end.len()]).collect();
            zero_starts.iter().map(Vec::as_slice).collect()
        } else {
            starts
        };

        let state_slot = &mut states[component_index * resolutions + usize::from(resolution)];
        let state = state_slot.get_or_insert_with(|| PacketHeaderState::new(&bands));
        let header = build_layer_packet_header(&bands, &starts, &ends, layer, state);
        let mut body = Vec::new();
        for (packet_band_index, &band_index) in band_indices.iter().enumerate() {
            let band = &layout.bands[band_index];
            for (block_index, block) in band.blocks.iter().enumerate() {
                let start =
                    usize::from(starts[packet_band_index][block_index]).min(block.passes.len());
                let end = usize::from(ends[packet_band_index][block_index])
                    .min(block.passes.len())
                    .max(start);
                if end == start {
                    continue;
                }
                let byte_start = if start == 0 {
                    0
                } else {
                    block.passes[start - 1].cumulative_length
                };
                let byte_end = block.passes[end - 1].cumulative_length;
                if byte_end > block.payload.len() || byte_start > byte_end {
                    return Err(Jp2LamError::EncodeFailed(format!(
                        "stored block {:?} pass range {byte_start}..{byte_end} exceeds payload length {}",
                        block.id,
                        block.payload.len()
                    )));
                }
                if byte_end > byte_start {
                    body.push(StoredPayloadRange {
                        payload: block.payload,
                        start: byte_start,
                        len: byte_end - byte_start,
                    });
                }
            }
        }
        packets.push(StoredPacket { header, body });
    }

    Ok(TilePartPayload::from_stored_packets(store, packets))
}

fn validate_layouts_for_tile(tile_index: u16, layouts: &[NativeEncodedTier1Layout]) -> Result<()> {
    for (component_index, layout) in layouts.iter().enumerate() {
        for band in &layout.bands {
            for block in &band.blocks {
                if block.tile_index != tile_index {
                    return Err(Jp2LamError::EncodeFailed(format!(
                        "component {component_index} resolution {} {:?} code-block ({},{}) belongs to tile {}, not tile {tile_index}",
                        band.resolution, band.band, block.x0, block.y0, block.tile_index
                    )));
                }
            }
        }
    }
    Ok(())
}

fn build_packet_body_segments(
    band: &mut NativeEncodedTier1Band,
    selected_band: Option<&super::t1::NativeTier1SelectionBand>,
) -> Vec<Vec<u8>> {
    let mut segments = Vec::new();
    for (cblkno, cblk) in band.blocks.iter_mut().enumerate() {
        let selected_count = selected_pass_count(cblk, selected_band, cblkno);
        if selected_count > 0 {
            let last_pass = &cblk.passes[selected_count - 1];
            let selected_len = last_pass.cumulative_length.min(cblk.payload.len());
            cblk.payload.truncate(selected_len);
            segments.push(std::mem::take(&mut cblk.payload));
        }
    }
    segments
}

// ---------------------------------------------------------------------------
// Retained for tests that use encode_placeholder_tier1
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use crate::plan::ProgressionOrder;

    use super::{
        PacketBio, TagTree, build_packet_sequence_for_tile_components_with_selections,
        build_tile_part_payload, build_tile_part_payload_for_components,
        build_tile_part_payload_for_components_with_selections,
        build_tile_part_payload_for_tile_components_owned,
        build_tile_part_payload_for_tile_components_with_selections, codeblock_segments,
        floor_log2, length_increment, numpasses_codeword, put_commacode, put_numpasses,
    };
    use crate::encode::backend::native::NativeComponentCoefficients;
    use crate::encode::backend::native::layout::build_component_layout;
    use crate::encode::backend::native::t1::{
        NativeEncodedTier1Pass, NativeTier1PassCodingMode, NativeTier1PassKind,
        NativeTier1PassTermination, NativeTier1SelectionBand, NativeTier1SelectionLayout,
        analyze_component_layout, encode_placeholder_tier1,
    };
    use crate::plan::CodeBlockSize;

    fn mock_pass(pass_index: u16, length: usize, is_terminated: bool) -> NativeEncodedTier1Pass {
        NativeEncodedTier1Pass {
            kind: NativeTier1PassKind::Cleanup,
            bitplane: 0,
            pass_index,
            coding_mode: NativeTier1PassCodingMode::Mq,
            termination: NativeTier1PassTermination::TermAll,
            segmark: false,
            is_terminated,
            newly_significant: 0,
            significant_before: 0,
            length,
            cumulative_length: 0,
            distortion_hint: 0,
            mse_numerator: 0.0,
        }
    }

    fn encoded_fixture_with_tile_index(tile_index: u16) -> super::NativeEncodedTier1Layout {
        let coefficients = NativeComponentCoefficients {
            x0: 0,
            y0: 0,
            width: 4,
            height: 4,
            levels: 2,
            data: vec![-38, 36, 0, 16, 144, 0, 0, 16, 0, 0, 0, 0, 64, 64, 0, 0],
        };
        let layout = build_component_layout(
            &coefficients,
            CodeBlockSize {
                width: 64,
                height: 64,
            },
        )
        .expect("build layout");
        let analyzed = analyze_component_layout(&layout);
        let mut encoded = encode_placeholder_tier1(&analyzed);
        for band in &mut encoded.bands {
            for block in &mut band.blocks {
                block.tile_index = tile_index;
            }
        }
        encoded
    }

    #[test]
    fn bio_putbit_roundtrip_indicator_bit() {
        let mut bio = PacketBio::new();
        bio.putbit(1);
        let bytes = bio.flush();
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0] & 0x80, 0x80, "indicator bit should be MSB");
    }

    #[test]
    fn tagtree_single_leaf_encodes_inclusion() {
        let mut tree = TagTree::new(1, 1);
        tree.reset();
        tree.set_value(0, 0);
        let mut bio = PacketBio::new();
        tree.encode(&mut bio, 0, 1);
        let bytes = bio.flush();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn floor_log2_values() {
        assert_eq!(floor_log2(1), 0);
        assert_eq!(floor_log2(2), 1);
        assert_eq!(floor_log2(3), 1);
        assert_eq!(floor_log2(4), 2);
        assert_eq!(floor_log2(127), 6);
        assert_eq!(floor_log2(128), 7);
    }

    #[test]
    fn numpasses_writes_1_as_0_bit() {
        let mut bio = PacketBio::new();
        put_numpasses(&mut bio, 1);
        let bytes = bio.flush();
        assert_eq!(bytes[0] & 0x80, 0, "numpasses=1 should write a 0 bit first");
    }

    #[test]
    fn numpasses_writes_2_as_10() {
        let mut bio = PacketBio::new();
        put_numpasses(&mut bio, 2);
        let bytes = bio.flush();
        assert_eq!(
            bytes[0] >> 6,
            0b10,
            "numpasses=2 should write 10 in top 2 bits"
        );
    }

    #[test]
    fn b106_numpasses_boundary_values_follow_table_b4() {
        let cases = [
            (3, 4, 0b1100u16),
            (5, 4, 0b1110u16),
            (6, 9, 0b1111_00000u16),
            (36, 9, 0b1111_11110u16),
            (37, 16, 0b11111111_10000000u16),
            (164, 16, 0b11111111_11111111u16),
        ];

        for (numpasses, width, expected_prefix) in cases {
            let (actual, actual_width) = numpasses_codeword(numpasses);
            assert_eq!(
                (actual, actual_width),
                (expected_prefix as u32, width),
                "B.10.6 codeword mismatch for numpasses={numpasses}"
            );
        }
    }

    #[test]
    fn b107_commacode_zero_writes_single_0() {
        let mut bio = PacketBio::new();
        put_commacode(&mut bio, 0);
        let bytes = bio.flush();
        assert_eq!(
            bytes[0] & 0x80,
            0,
            "B.10.7 commacode(0) should write a 0 bit"
        );
    }

    #[test]
    fn b1071_single_segment_uses_initial_lblock_three() {
        let segments = codeblock_segments(&[mock_pass(0, 6, true)]);
        assert_eq!(segments, vec![(6, 1)]);
        assert_eq!(length_increment(3, &segments), 0);
    }

    #[test]
    fn b1072_multiple_segments_match_standard_note_partition() {
        // ISO/IEC 15444-1 Annex B.10.7.2 note:
        // pass lengths {6, 31, 44, 134, 192} with terminated-pass set
        // T = {0, 2, 3, 4} yields signaled segment lengths {6, 75, 134, 192}
        // and added-pass counts {1, 2, 1, 1}.
        let passes = vec![
            mock_pass(0, 6, true),
            mock_pass(1, 31, false),
            mock_pass(2, 44, true),
            mock_pass(3, 134, true),
            mock_pass(4, 192, true),
        ];
        let segments = codeblock_segments(&passes);
        assert_eq!(segments, vec![(6, 1), (75, 2), (134, 1), (192, 1)]);
        assert_eq!(length_increment(3, &segments), 5);
    }

    #[test]
    fn tile_part_payload_has_correct_packet_count() {
        let coefficients = NativeComponentCoefficients {
            x0: 0,
            y0: 0,
            width: 4,
            height: 4,
            levels: 2,
            data: vec![-38, 36, 0, 16, 144, 0, 0, 16, 0, 0, 0, 0, 64, 64, 0, 0],
        };
        let layout = build_component_layout(
            &coefficients,
            CodeBlockSize {
                width: 64,
                height: 64,
            },
        )
        .expect("build layout");
        let analyzed = analyze_component_layout(&layout);
        let encoded = encode_placeholder_tier1(&analyzed);
        let payload = build_tile_part_payload(&encoded).expect("build tile part payload");
        assert_eq!(payload.packet_count(), 3);
    }

    #[test]
    fn tile_part_payload_headers_start_with_indicator_bit() {
        let coefficients = NativeComponentCoefficients {
            x0: 0,
            y0: 0,
            width: 4,
            height: 4,
            levels: 2,
            data: vec![-38, 36, 0, 16, 144, 0, 0, 16, 0, 0, 0, 0, 64, 64, 0, 0],
        };
        let layout = build_component_layout(
            &coefficients,
            CodeBlockSize {
                width: 64,
                height: 64,
            },
        )
        .expect("build layout");
        let analyzed = analyze_component_layout(&layout);
        let encoded = encode_placeholder_tier1(&analyzed);
        let payload = build_tile_part_payload(&encoded).expect("build tile part payload");
        let mut out = Vec::new();
        payload.write_to(&mut out).expect("write payload");
        assert!(!out.is_empty());
    }

    #[test]
    fn tile_part_payload_for_explicit_tile_accepts_matching_layouts() {
        let encoded = encoded_fixture_with_tile_index(3);

        let borrowed = build_tile_part_payload_for_tile_components_with_selections(
            3,
            std::slice::from_ref(&encoded),
            None,
        )
        .expect("borrowed tile payload");
        assert_eq!(borrowed.packet_count(), 3);

        let owned = build_tile_part_payload_for_tile_components_owned(
            3,
            vec![encoded],
            None,
            ProgressionOrder::Lrcp,
        )
        .expect("owned tile payload");
        assert_eq!(owned.packet_count(), 3);
    }

    #[test]
    fn tile_part_payload_for_explicit_tile_rejects_mismatched_layouts() {
        let encoded = encoded_fixture_with_tile_index(4);

        let err = build_packet_sequence_for_tile_components_with_selections(
            3,
            std::slice::from_ref(&encoded),
            None,
        )
        .expect_err("packet sequence should reject wrong tile");
        assert!(err.to_string().contains("not tile 3"), "{err}");

        let owned_err = build_tile_part_payload_for_tile_components_owned(
            3,
            vec![encoded],
            None,
            ProgressionOrder::Lrcp,
        )
        .expect_err("owned payload should reject wrong tile");
        assert!(owned_err.to_string().contains("not tile 3"), "{owned_err}");
    }

    #[test]
    fn selection_table_limits_payload_without_mutating_encoded_layout() {
        let coefficients = NativeComponentCoefficients {
            x0: 0,
            y0: 0,
            width: 4,
            height: 4,
            levels: 2,
            data: vec![-38, 36, 0, 16, 144, 0, 0, 16, 0, 0, 0, 0, 64, 64, 0, 0],
        };
        let layout = build_component_layout(
            &coefficients,
            CodeBlockSize {
                width: 64,
                height: 64,
            },
        )
        .expect("build layout");
        let analyzed = analyze_component_layout(&layout);
        let encoded = encode_placeholder_tier1(&analyzed);
        let original = encoded.clone();
        let selection = NativeTier1SelectionLayout {
            bands: encoded
                .bands
                .iter()
                .map(|band| NativeTier1SelectionBand {
                    resolution: band.resolution,
                    band: band.band,
                    selected_passes: band
                        .blocks
                        .iter()
                        .map(|block| if block.passes.is_empty() { 0 } else { 1 })
                        .collect(),
                })
                .collect(),
        };

        let full = build_tile_part_payload_for_components(std::slice::from_ref(&encoded))
            .expect("full payload");
        let selected = build_tile_part_payload_for_components_with_selections(
            std::slice::from_ref(&encoded),
            Some(std::slice::from_ref(&selection)),
        )
        .expect("selected payload");

        let mut full_bytes = Vec::new();
        full.write_to(&mut full_bytes).expect("write full payload");
        let mut selected_bytes = Vec::new();
        selected
            .write_to(&mut selected_bytes)
            .expect("write selected payload");

        assert!(selected_bytes.len() <= full_bytes.len());
        assert_eq!(
            encoded, original,
            "selection must not mutate encoded layout"
        );
    }

    #[test]
    fn packet_with_no_selected_passes_uses_empty_packet_bit() {
        let encoded = encoded_fixture_with_tile_index(0);
        let bands = encoded.bands.iter().collect::<Vec<_>>();
        let selection = encoded
            .bands
            .iter()
            .map(|band| NativeTier1SelectionBand {
                resolution: band.resolution,
                band: band.band,
                selected_passes: vec![0; band.blocks.len()],
            })
            .collect::<Vec<_>>();
        let selected_bands = selection.iter().collect::<Vec<_>>();

        assert_eq!(
            super::build_packet_header(&bands, Some(&selected_bands)),
            vec![0x00]
        );
    }
}
