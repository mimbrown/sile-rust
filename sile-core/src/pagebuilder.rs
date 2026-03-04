use crate::frame::{FrameId, PageLayout};
use crate::node::Node;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const INF_BAD: i64 = 10_000;
const EJECT_PENALTY: i32 = -10_000;
const INF_PENALTY: i32 = 10_000;
const AWFUL_BAD: i64 = 1_073_741_823;

// ---------------------------------------------------------------------------
// PageBreakSettings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PageBreakSettings {
    pub tolerance: i64,
    pub line_penalty: i64,
    pub widow_penalty: i32,
    pub orphan_penalty: i32,
    pub club_penalty: i32,
    pub inter_line_penalty: i32,
    pub broken_penalty: i32,
    pub pre_display_penalty: i32,
    pub post_display_penalty: i32,
}

impl Default for PageBreakSettings {
    fn default() -> Self {
        Self {
            tolerance: 500,
            line_penalty: 10,
            widow_penalty: 150,
            orphan_penalty: 150,
            club_penalty: 150,
            inter_line_penalty: 0,
            broken_penalty: 100,
            pre_display_penalty: 10_000,
            post_display_penalty: 10_000,
        }
    }
}

// ---------------------------------------------------------------------------
// PageBreakResult
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PageBreakResult {
    pub break_index: usize,
    pub badness: i64,
    pub penalty: i32,
    pub cost: i64,
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Page {
    pub number: usize,
    pub frames: Vec<(FrameId, Vec<Node>)>,
}

impl Page {
    pub fn new(number: usize) -> Self {
        Self {
            number,
            frames: Vec::new(),
        }
    }

    pub fn add_frame_content(&mut self, id: FrameId, nodes: Vec<Node>) {
        self.frames.push((id, nodes));
    }
}

// ---------------------------------------------------------------------------
// Vertical badness / cost
// ---------------------------------------------------------------------------

fn v_badness(shortfall: f64, stretch: f64) -> i64 {
    if stretch == 0.0 {
        if shortfall.abs() < 0.1 {
            return 0;
        }
        return INF_BAD;
    }
    let bad = (100.0 * (shortfall / stretch).abs().powi(3)).floor() as i64;
    bad.min(INF_BAD)
}

fn page_cost(badness: i64, penalty: i32, page_count: usize) -> i64 {
    let p = penalty as i64;
    if penalty < EJECT_PENALTY + 1 {
        return p;
    }
    let b = badness;
    let base = if b < INF_BAD {
        b * b + p * p.abs()
    } else {
        AWFUL_BAD
    };
    let _ = page_count;
    base
}

// ---------------------------------------------------------------------------
// PageBuilder
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct PageBuilder {
    pub settings: PageBreakSettings,
    queue: Vec<Node>,
    pages: Vec<Page>,
    page_number: usize,
}

impl PageBuilder {
    pub fn new(settings: PageBreakSettings) -> Self {
        Self {
            settings,
            queue: Vec::new(),
            pages: Vec::new(),
            page_number: 0,
        }
    }

    pub fn enqueue(&mut self, node: Node) {
        self.queue.push(node);
    }

    pub fn enqueue_many(&mut self, nodes: impl IntoIterator<Item = Node>) {
        self.queue.extend(nodes);
    }

    pub fn queue(&self) -> &[Node] {
        &self.queue
    }

    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    pub fn into_pages(self) -> Vec<Page> {
        self.pages
    }

    /// Inject widow/orphan penalty nodes into a VBox node list.
    pub fn inject_penalties(nodes: &mut Vec<Node>, settings: &PageBreakSettings) {
        let vbox_count = nodes.iter().filter(|n| n.is_vbox()).count();
        if vbox_count < 3 {
            return;
        }

        // Find indices of VBox nodes
        let vbox_indices: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.is_vbox())
            .map(|(i, _)| i)
            .collect();

        let mut insertions = Vec::new();

        // Orphan penalty: after first VBox (prevent first line alone at bottom)
        if settings.orphan_penalty != 0 && vbox_indices.len() >= 2 {
            insertions.push((vbox_indices[0] + 1, settings.orphan_penalty));
        }

        // Club penalty: after second VBox
        if settings.club_penalty != 0 && vbox_indices.len() >= 3 {
            insertions.push((vbox_indices[1] + 1, settings.club_penalty));
        }

        // Widow penalty: before last VBox (prevent last line alone at top)
        if settings.widow_penalty != 0 && vbox_indices.len() >= 2 {
            let before_last = *vbox_indices.last().unwrap();
            // Insert penalty before the last vbox (find the glue before it)
            if before_last > 0 {
                insertions.push((before_last, settings.widow_penalty));
            }
        }

        // Sort insertions by position descending to avoid index shifting
        insertions.sort_by(|a, b| b.0.cmp(&a.0));
        insertions.dedup_by_key(|x| x.0);

        for (idx, penalty_val) in insertions {
            if idx <= nodes.len() {
                nodes.insert(idx, Node::penalty(penalty_val));
            }
        }
    }

    /// Find the best page break point in the current queue for a given target height.
    ///
    /// Follows TeX's page-breaking rules: legal break points are at
    /// penalties, and at glue that follows a box or vbox.
    pub fn find_break(&self, target_height: f64) -> Option<PageBreakResult> {
        if self.queue.is_empty() {
            return None;
        }

        let mut height = 0.0_f64;
        let mut stretch = 0.0_f64;
        let mut shrink = 0.0_f64;
        let mut best: Option<PageBreakResult> = None;
        let mut prev_was_box = false;

        for (i, node) in self.queue.iter().enumerate() {
            // Check for legal break point BEFORE adding this node's dimensions.
            // TeX rule: glue after a box is a legal break point (penalty 0).
            let is_legal_break = if node.is_penalty() {
                matches!(node, Node::Penalty(p) if p.penalty < INF_PENALTY)
            } else {
                node.is_vglue() && prev_was_box
            };

            if is_legal_break {
                let pi = if let Node::Penalty(p) = node {
                    p.penalty
                } else {
                    0 // natural glue break
                };

                let shortfall = target_height - height;
                let badness = if shortfall >= 0.0 {
                    v_badness(shortfall, stretch)
                } else {
                    let excess = -shortfall;
                    if excess > shrink {
                        INF_BAD + 1
                    } else {
                        v_badness(excess, shrink)
                    }
                };

                let cost = page_cost(badness, pi, self.page_number);

                let is_better = match &best {
                    None => true,
                    Some(prev) => {
                        pi <= EJECT_PENALTY
                            || cost < prev.cost
                            || (cost == prev.cost && badness < prev.badness)
                    }
                };

                if is_better {
                    best = Some(PageBreakResult {
                        break_index: i,
                        badness,
                        penalty: pi,
                        cost,
                    });
                }

                if pi <= EJECT_PENALTY {
                    return best;
                }

                // If we've overflowed past the target, stop searching —
                // the best break we've found so far is optimal.
                if height > target_height + shrink && best.is_some() {
                    return best;
                }
            }

            // Accumulate dimensions
            match node {
                Node::VBox(_) => {
                    let h = node.height().to_pt().unwrap_or(0.0);
                    let d = node.depth().to_pt().unwrap_or(0.0);
                    height += h + d;
                    prev_was_box = true;
                }
                Node::VGlue(g) | Node::VFillGlue(g) | Node::VssGlue(g) | Node::ZeroVGlue(g) => {
                    let h = g.height.length.to_pt().unwrap_or(0.0);
                    let st = g.height.stretch.to_pt().unwrap_or(0.0);
                    let sh = g.height.shrink.to_pt().unwrap_or(0.0);
                    height += h;
                    stretch += st;
                    shrink += sh;
                    prev_was_box = false;
                }
                Node::VKern(_) => {
                    let h = node.height().to_pt().unwrap_or(0.0);
                    height += h.abs();
                    prev_was_box = false;
                }
                Node::Penalty(_) => {
                    let h = node.height().to_pt().unwrap_or(0.0);
                    height += h;
                    prev_was_box = false;
                }
                _ => {
                    let h = node.height().to_pt().unwrap_or(0.0);
                    let d = node.depth().to_pt().unwrap_or(0.0);
                    height += h + d;
                    prev_was_box = node.is_box();
                }
            }
        }

        // If we exhausted the queue without a forced break, check if we have
        // enough content for a page
        if best.is_none() && height > 0.0 {
            let shortfall = target_height - height;
            if shortfall <= 0.0 || height > target_height * 0.5 {
                let badness = if shortfall >= 0.0 {
                    v_badness(shortfall, stretch)
                } else {
                    INF_BAD
                };
                let cost = page_cost(badness, 0, self.page_number);
                best = Some(PageBreakResult {
                    break_index: self.queue.len().saturating_sub(1),
                    badness,
                    penalty: 0,
                    cost,
                });
            }
        }

        best
    }

    /// Build pages from the queue, distributing content into the target frame.
    pub fn build_pages(&mut self, layout: &PageLayout, target_frame: FrameId) -> Vec<Page> {
        let target_height = layout.frame(target_frame).height();
        let mut result_pages = Vec::new();

        loop {
            if self.queue.is_empty() {
                break;
            }

            let break_result = match self.find_break(target_height) {
                Some(br) => br,
                None => break,
            };

            self.page_number += 1;
            let mut page = Page::new(self.page_number);

            // Split at break point
            let break_idx = break_result.break_index;
            let (page_nodes, remaining) = if break_idx + 1 >= self.queue.len() {
                (std::mem::take(&mut self.queue), Vec::new())
            } else {
                let remaining = self.queue.split_off(break_idx + 1);
                (std::mem::take(&mut self.queue), remaining)
            };

            // Trim discardable nodes from the end of the page
            let mut content: Vec<Node> = page_nodes;
            while content.last().is_some_and(|n| n.is_discardable()) {
                content.pop();
            }

            page.add_frame_content(target_frame, content);
            result_pages.push(page);

            // Trim discardable nodes from the start of remaining
            self.queue = remaining;
            while self.queue.first().is_some_and(|n| n.is_discardable()) {
                self.queue.remove(0);
            }

            // If last break was forced but we have no more content, stop
            if self.queue.is_empty() {
                break;
            }
        }

        // If there's leftover content that didn't fill a page, flush it
        if !self.queue.is_empty() {
            self.page_number += 1;
            let mut page = Page::new(self.page_number);
            let content = std::mem::take(&mut self.queue);
            page.add_frame_content(target_frame, content);
            result_pages.push(page);
        }

        self.pages.extend(result_pages.clone());
        result_pages
    }

    /// Build pages using a multi-frame layout, distributing content across
    /// chained frames. Content flows from one frame to the next via `frame.next`.
    pub fn build_pages_multi_frame(
        &mut self,
        layout: &PageLayout,
        start_frame: FrameId,
    ) -> Vec<Page> {
        let mut result_pages = Vec::new();

        loop {
            if self.queue.is_empty() {
                break;
            }

            self.page_number += 1;
            let mut page = Page::new(self.page_number);
            let mut current_frame = start_frame;

            loop {
                let frame = layout.frame(current_frame);
                let target_height = frame.height();

                if self.queue.is_empty() {
                    break;
                }

                let break_result = match self.find_break(target_height) {
                    Some(br) => br,
                    None => break,
                };

                let break_idx = break_result.break_index;
                let (page_nodes, remaining) = if break_idx + 1 >= self.queue.len() {
                    (std::mem::take(&mut self.queue), Vec::new())
                } else {
                    let remaining = self.queue.split_off(break_idx + 1);
                    (std::mem::take(&mut self.queue), remaining)
                };

                let mut content: Vec<Node> = page_nodes;
                while content.last().is_some_and(|n| n.is_discardable()) {
                    content.pop();
                }

                page.add_frame_content(current_frame, content);

                self.queue = remaining;
                while self.queue.first().is_some_and(|n| n.is_discardable()) {
                    self.queue.remove(0);
                }

                // Follow frame chain
                match frame.next {
                    Some(next_id) if !self.queue.is_empty() => {
                        current_frame = next_id;
                    }
                    _ => break,
                }
            }

            result_pages.push(page);

            if self.queue.is_empty() {
                break;
            }
        }

        self.pages.extend(result_pages.clone());
        result_pages
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::PaperSize;
    use crate::length::Length;
    use crate::measurement::Measurement;
    use crate::node::VBox;

    fn make_line(height: f64, depth: f64) -> Node {
        Node::VBox(VBox {
            width: Length::pt(300.0),
            height: Length::pt(height),
            depth: Length::pt(depth),
            nodes: vec![Node::hbox(300.0, height, depth)],
            ratio: 0.0,
            misfit: false,
            explicit: false,
        })
    }

    fn make_vglue(height: f64) -> Node {
        Node::vglue(Length::new(
            Measurement::pt(height),
            Measurement::pt(height * 0.5),
            Measurement::pt(height * 0.3),
        ))
    }

    fn make_paragraph(num_lines: usize, line_height: f64, line_depth: f64) -> Vec<Node> {
        let mut nodes = Vec::new();
        for i in 0..num_lines {
            if i > 0 {
                nodes.push(make_vglue(2.0));
            }
            nodes.push(make_line(line_height, line_depth));
        }
        nodes
    }

    // -- v_badness --

    #[test]
    fn badness_zero_shortfall() {
        assert_eq!(v_badness(0.0, 10.0), 0);
    }

    #[test]
    fn badness_zero_stretch() {
        assert_eq!(v_badness(10.0, 0.0), INF_BAD);
    }

    #[test]
    fn badness_near_zero_shortfall() {
        assert_eq!(v_badness(0.05, 0.0), 0);
    }

    #[test]
    fn badness_ratio_one() {
        assert_eq!(v_badness(10.0, 10.0), 100);
    }

    // -- page_cost --

    #[test]
    fn cost_with_eject_penalty() {
        let cost = page_cost(0, EJECT_PENALTY, 1);
        assert_eq!(cost, EJECT_PENALTY as i64);
    }

    #[test]
    fn cost_normal() {
        let cost = page_cost(50, 0, 1);
        assert_eq!(cost, 2500); // 50*50
    }

    // -- PageBuilder find_break --

    #[test]
    fn find_break_empty() {
        let pb = PageBuilder::new(PageBreakSettings::default());
        assert!(pb.find_break(600.0).is_none());
    }

    #[test]
    fn find_break_at_penalty() {
        let mut pb = PageBuilder::new(PageBreakSettings::default());
        pb.enqueue(make_line(12.0, 3.0));
        pb.enqueue(make_vglue(2.0));
        pb.enqueue(make_line(12.0, 3.0));
        pb.enqueue(Node::penalty(0));
        pb.enqueue(make_line(12.0, 3.0));

        let result = pb.find_break(35.0).unwrap();
        assert_eq!(result.break_index, 3); // at the penalty
    }

    #[test]
    fn find_break_forced_eject() {
        let mut pb = PageBuilder::new(PageBreakSettings::default());
        pb.enqueue(make_line(12.0, 3.0));
        pb.enqueue(Node::penalty(EJECT_PENALTY));
        pb.enqueue(make_line(12.0, 3.0));

        let result = pb.find_break(600.0).unwrap();
        assert_eq!(result.penalty, EJECT_PENALTY);
        assert_eq!(result.break_index, 1);
    }

    #[test]
    fn find_break_no_break_at_inf_penalty() {
        let mut pb = PageBuilder::new(PageBreakSettings::default());
        pb.enqueue(make_line(12.0, 3.0));
        pb.enqueue(Node::penalty(INF_PENALTY));
        pb.enqueue(make_line(12.0, 3.0));
        pb.enqueue(Node::penalty(0));

        let result = pb.find_break(600.0).unwrap();
        // Should skip the INF_PENALTY and break at the 0 penalty
        assert_eq!(result.break_index, 3);
    }

    // -- PageBuilder build_pages --

    #[test]
    fn build_single_page() {
        let mut pb = PageBuilder::new(PageBreakSettings::default());
        let nodes = make_paragraph(5, 12.0, 3.0);
        pb.enqueue_many(nodes);
        // Add an eject penalty to force page
        pb.enqueue(Node::penalty(EJECT_PENALTY));

        let layout = PageLayout::plain(PaperSize::A4, 72.0);
        let frame_id = layout.content_frame_id().unwrap();
        let pages = pb.build_pages(&layout, frame_id);

        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].number, 1);
        assert_eq!(pages[0].frames.len(), 1);
        assert_eq!(pages[0].frames[0].0, frame_id);
    }

    #[test]
    fn build_multiple_pages() {
        let mut pb = PageBuilder::new(PageBreakSettings::default());

        // Create enough lines to fill ~3 pages
        // A4 content height with 72pt margin ≈ 698pt
        // Each line: 12pt + 3pt + 2pt glue = 17pt → ~41 lines per page
        let nodes = make_paragraph(120, 12.0, 3.0);
        for node in &nodes {
            pb.enqueue(node.clone());
        }
        // Force final page
        pb.enqueue(Node::penalty(EJECT_PENALTY));

        let layout = PageLayout::plain(PaperSize::A4, 72.0);
        let frame_id = layout.content_frame_id().unwrap();
        let pages = pb.build_pages(&layout, frame_id);

        assert!(
            pages.len() >= 2,
            "expected at least 2 pages, got {}",
            pages.len()
        );

        for page in &pages {
            assert_eq!(page.frames.len(), 1);
        }
    }

    #[test]
    fn build_pages_trims_discardables() {
        let mut pb = PageBuilder::new(PageBreakSettings::default());
        pb.enqueue(make_line(12.0, 3.0));
        pb.enqueue(make_vglue(2.0));
        pb.enqueue(Node::penalty(EJECT_PENALTY));
        pb.enqueue(make_vglue(2.0)); // leading discardable on next page
        pb.enqueue(make_line(12.0, 3.0));
        pb.enqueue(Node::penalty(EJECT_PENALTY));

        let layout = PageLayout::plain(PaperSize::A4, 72.0);
        let frame_id = layout.content_frame_id().unwrap();
        let pages = pb.build_pages(&layout, frame_id);

        assert_eq!(pages.len(), 2);

        // First page: vglue and penalty trimmed from end
        let p1_content = &pages[0].frames[0].1;
        assert!(
            p1_content.last().unwrap().is_vbox(),
            "last node on page should be a vbox after trimming"
        );

        // Second page: leading vglue should be trimmed
        let p2_content = &pages[1].frames[0].1;
        assert!(
            p2_content.first().unwrap().is_vbox(),
            "first node on page should be a vbox after trimming"
        );
    }

    // -- inject_penalties --

    #[test]
    fn inject_widow_orphan_penalties() {
        let mut nodes = make_paragraph(5, 12.0, 3.0);
        let original_len = nodes.len();

        PageBuilder::inject_penalties(&mut nodes, &PageBreakSettings::default());

        // Should have inserted penalty nodes
        assert!(nodes.len() > original_len);

        // Check that penalties exist in the list
        let penalty_count = nodes.iter().filter(|n| n.is_penalty()).count();
        assert!(penalty_count >= 2, "should have at least 2 penalty nodes");
    }

    #[test]
    fn inject_penalties_short_paragraph() {
        let mut nodes = make_paragraph(2, 12.0, 3.0);
        let original_len = nodes.len();

        PageBuilder::inject_penalties(&mut nodes, &PageBreakSettings::default());

        // Too few vboxes, no penalties injected
        assert_eq!(nodes.len(), original_len);
    }

    // -- Multi-frame page building --

    #[test]
    fn build_pages_multi_frame_with_columns() {
        let mut pb = PageBuilder::new(PageBreakSettings::default());

        // Create 80 lines (~80×17pt = 1360pt). Each column is ~698pt tall,
        // so content should overflow into the second column.
        let nodes = make_paragraph(80, 12.0, 3.0);
        for node in &nodes {
            pb.enqueue(node.clone());
        }
        pb.enqueue(Node::penalty(EJECT_PENALTY));

        let layout = PageLayout::two_column(PaperSize::A4, 72.0, 0.0, 0.0, 0.0, 12.0);
        let ids = layout.frame_ids();
        // ids[1] = left_column, ids[2] = right_column (left flows to right)
        let start = ids[1];

        let pages = pb.build_pages_multi_frame(&layout, start);

        assert!(!pages.is_empty());

        // At least one page should have content in both columns
        let multi_frame_page = pages.iter().find(|p| p.frames.len() >= 2);
        assert!(
            multi_frame_page.is_some(),
            "at least one page should use both columns"
        );
    }

    #[test]
    fn build_pages_header_footer() {
        let mut pb = PageBuilder::new(PageBreakSettings::default());

        // Enough content for ~2 pages of the body frame
        let nodes = make_paragraph(80, 12.0, 3.0);
        pb.enqueue_many(nodes);
        pb.enqueue(Node::penalty(EJECT_PENALTY));

        let layout =
            PageLayout::with_header_footer(PaperSize::A4, 72.0, 30.0, 20.0, 10.0);
        let content_id = layout.content_frame_id().unwrap();

        let pages = pb.build_pages(&layout, content_id);

        assert!(
            pages.len() >= 2,
            "expected at least 2 pages, got {}",
            pages.len()
        );
    }

    // -- End-to-end: paragraph → page break --

    #[test]
    fn end_to_end_paragraph_to_pages() {
        let mut pb = PageBuilder::new(PageBreakSettings::default());

        // Simulate 3 paragraphs with inter-paragraph glue and penalty
        for _para in 0..3 {
            let mut lines = make_paragraph(15, 12.0, 3.0);
            PageBuilder::inject_penalties(&mut lines, &pb.settings);
            pb.enqueue_many(lines);
            pb.enqueue(make_vglue(6.0)); // paragraph skip
            pb.enqueue(Node::penalty(0)); // allow break between paragraphs
        }
        pb.enqueue(Node::penalty(EJECT_PENALTY));

        let layout = PageLayout::plain(PaperSize::A4, 72.0);
        let frame_id = layout.content_frame_id().unwrap();
        let pages = pb.build_pages(&layout, frame_id);

        assert!(!pages.is_empty());
        // Verify all pages have content
        for page in &pages {
            assert!(!page.frames.is_empty());
            assert!(!page.frames[0].1.is_empty());
        }
    }
}
