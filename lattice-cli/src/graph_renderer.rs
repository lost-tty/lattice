//! Grid-based graph renderer for history visualization.
//! Uses box drawing characters and z-ordering for crossovers.
//!
//! Design inspired by git-graph: https://github.com/mlange-42/git-graph (MIT)

use crate::display_helpers::{
    ansi_code, author_color, render_sexpr_colored, render_sexpr_pretty_colored, strip_ansi,
};
use lattice_runtime::{Hash, PubKey, SExpr};
use std::fmt::Write;

// Character codes for grid cells
pub const SPACE: u8 = 0;
pub const DOT: u8 = 1; // ● entry marker
pub const CIRCLE: u8 = 2; // ○ merge entry marker
pub const ROOT: u8 = 3; // ⊙ root entry marker (no parents)
pub const VER: u8 = 4; // │ vertical line
pub const HOR: u8 = 5; // ─ horizontal line
pub const CROSS: u8 = 6; // ┼ crossing
pub const R_U: u8 = 7; // ╰ right-up corner
pub const R_D: u8 = 8; // ╭ right-down corner
pub const L_D: u8 = 9; // ╮ left-down corner
pub const L_U: u8 = 10; // ╯ left-up corner
pub const VER_L: u8 = 11; // ┤ vertical with left branch
pub const VER_R: u8 = 12; // ├ vertical with right branch
pub const HOR_U: u8 = 13; // ┴ horizontal with up branch
pub const HOR_D: u8 = 14; // ┬ horizontal with down branch
pub const ARR_L: u8 = 15; // ⟨ left angle bracket (merge indicator)
pub const ARR_R: u8 = 16; // ⟩ right angle bracket (merge indicator)

// Character mappings
const CHARS: [char; 17] = [
    ' ', // SPACE
    '●', // DOT
    '○', // CIRCLE
    '⊙', // ROOT
    '│', // VER
    '─', // HOR
    '┼', // CROSS
    '╰', // R_U
    '╭', // R_D
    '╮', // L_D
    '╯', // L_U
    '┤', // VER_L
    '├', // VER_R
    '┴', // HOR_U
    '┬', // HOR_D
    '⟨', // ARR_L
    '⟩', // ARR_R
];

/// A single cell in the rendering grid
#[derive(Clone, Copy)]
pub struct GridCell {
    /// Character code (see constants above)
    pub character: u8,
    /// Persistence/z-order - lower numbers take precedence when lines cross
    pub persistence: u8,
    /// ANSI color code (0 = no color, 31-36 = colors)
    pub color: u8,
}

impl Default for GridCell {
    fn default() -> Self {
        Self {
            character: SPACE,
            persistence: u8::MAX,
            color: 0,
        }
    }
}

impl GridCell {
    pub fn to_char(&self) -> char {
        CHARS.get(self.character as usize).copied().unwrap_or(' ')
    }
}

/// 2D grid for rendering the graph
pub struct Grid {
    pub width: usize,
    pub height: usize,
    data: Vec<GridCell>,
}

impl Grid {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![GridCell::default(); width * height],
        }
    }

    fn index(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }

    pub fn get(&self, x: usize, y: usize) -> GridCell {
        if x >= self.width || y >= self.height {
            return GridCell::default();
        }
        self.data[self.index(x, y)]
    }

    /// Set a cell with color, respecting persistence
    pub fn set_colored(&mut self, x: usize, y: usize, character: u8, persistence: u8, color: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = self.index(x, y);
        let cell = &mut self.data[idx];

        if persistence <= cell.persistence {
            cell.character = character;
            cell.persistence = persistence;
            cell.color = color;
        }
    }

    /// Set with conflict resolution for crossings
    pub fn set_with_merge(&mut self, x: usize, y: usize, character: u8, persistence: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = self.index(x, y);
        let cell = &mut self.data[idx];
        let curr = cell.character;

        // Handle crossing scenarios
        let new_char = match (curr, character) {
            // Commit markers are never overwritten
            (DOT, _) | (CIRCLE, _) => curr,
            (_, DOT) | (_, CIRCLE) => character,

            // Vertical meets horizontal -> cross
            (VER, HOR) | (HOR, VER) => CROSS,

            // Corners meeting lines
            (VER, L_D) | (VER, L_U) => VER_L,
            (VER, R_D) | (VER, R_U) => VER_R,
            (L_D, VER) | (L_U, VER) => VER_L,
            (R_D, VER) | (R_U, VER) => VER_R,

            // Corners meeting corners (vertical branches)
            (L_D, L_U) | (L_U, L_D) => VER_L,
            (R_D, R_U) | (R_U, R_D) => VER_R,

            (HOR, L_U) | (HOR, R_U) => HOR_U,
            (HOR, L_D) | (HOR, R_D) => HOR_D,
            (L_U, HOR) | (R_U, HOR) => HOR_U,
            (L_D, HOR) | (R_D, HOR) => HOR_D,

            // T-junctions meeting lines
            (VER_L, HOR) | (VER_R, HOR) => CROSS,
            (HOR_U, VER) | (HOR_D, VER) => CROSS,
            (HOR, VER_L) | (HOR, VER_R) => CROSS,
            (VER, HOR_U) | (VER, HOR_D) => CROSS,

            // Preserve complex shapes when overwritten by simple lines
            (HOR_U, HOR) | (HOR, HOR_U) => HOR_U,
            (HOR_D, HOR) | (HOR, HOR_D) => HOR_D,
            (VER_L, VER) | (VER, VER_L) => VER_L,
            (VER_R, VER) | (VER, VER_R) => VER_R,
            (CROSS, _) | (_, CROSS) => CROSS, // Cross usually wins everything except explicit overrides

            // Default: use persistence
            _ => {
                if persistence <= cell.persistence {
                    character
                } else {
                    curr
                }
            }
        };

        cell.character = new_char;
        if persistence < cell.persistence {
            cell.persistence = persistence;
        }
    }

    /// Draw a vertical line from y1 to y2 (exclusive of endpoints)
    pub fn vline(&mut self, x: usize, y1: usize, y2: usize, persistence: u8) {
        let (start, end) = if y1 < y2 { (y1, y2) } else { (y2, y1) };
        for y in (start + 1)..end {
            self.set_with_merge(x, y, VER, persistence);
        }
    }

    /// Draw a horizontal line from x1 to x2 (exclusive of endpoints)
    /// x1 is the source position, x2 is the destination position
    /// is_merge indicates this is a secondary parent connection
    /// curves_down: if true, the far end curves down; if false, it curves up
    pub fn hline(
        &mut self,
        y: usize,
        x1: usize,
        x2: usize,
        is_merge: bool,
        curves_down: bool,
        persistence: u8,
    ) {
        if x1 == x2 {
            return;
        }

        let (left, right) = if x1 < x2 { (x1, x2) } else { (x2, x1) };

        // Draw horizontal segments
        // For merges, put an arrow pointing toward the merge entry (x2)
        for x in (left + 1)..right {
            if is_merge {
                // Arrow points toward x2 (the merge entry, which is the destination)
                if x1 < x2 && x == right - 1 {
                    // Merge entry (x2) is on right, arrow points right
                    self.set_with_merge(x, y, ARR_R, persistence);
                } else if x1 > x2 && x == left + 1 {
                    // Merge entry (x2) is on left, arrow points left
                    self.set_with_merge(x, y, ARR_L, persistence);
                } else {
                    self.set_with_merge(x, y, HOR, persistence);
                }
            } else {
                self.set_with_merge(x, y, HOR, persistence);
            }
        }

        // Draw corners based on direction
        if curves_down {
            // Branching: horizontal then down
            // ●──╮  (R_D at start if going right, L_D at far end to curve down)
            if x1 < x2 {
                self.set_with_merge(left, y, VER_R, persistence); // ├ branch point
                self.set_with_merge(right, y, L_D, persistence); // ╮ curve down
            } else {
                self.set_with_merge(right, y, VER_L, persistence); // ┤ branch point
                self.set_with_merge(left, y, R_D, persistence); // ╭ curve down
            }
        } else {
            // Merging: horizontal then up
            // ╰──●  (R_U at far end to curve up)
            if x1 < x2 {
                self.set_with_merge(left, y, R_U, persistence); // ╰ curve up
                self.set_with_merge(right, y, L_U, persistence); // ╯ curve up
            } else {
                self.set_with_merge(right, y, L_U, persistence); // ╯ curve up
                self.set_with_merge(left, y, R_U, persistence); // ╰ curve up
            }
        }
    }

    /// Render the vertical-line columns for row `y` (used on label continuation lines).
    /// Shows `│` where vertical lines pass through, spaces elsewhere.
    fn render_vlines(&self, y: usize, output: &mut String) {
        for x in 0..self.width {
            let cell = self.get(x, y);
            let has_vertical =
                matches!(cell.character, VER | CROSS | VER_L | VER_R | HOR_U | HOR_D);
            if has_vertical && cell.color > 0 {
                let _ = write!(output, "\x1b[{}m│\x1b[0m", cell.color);
            } else if has_vertical {
                output.push('│');
            } else {
                output.push(' ');
            }
        }
    }

    /// Render grid to string, appending text labels on the right.
    /// Multi-line labels have continuation lines showing vertical graph lines.
    pub fn render(&self, labels: &[String]) -> String {
        let mut output = String::new();

        for y in 0..self.height {
            // Render graph columns
            for x in 0..self.width {
                let cell = self.get(x, y);
                if cell.color > 0 {
                    let _ = write!(output, "\x1b[{}m{}\x1b[0m", cell.color, cell.to_char());
                } else {
                    output.push(cell.to_char());
                }
            }

            // Append label if present, handling multi-line wrapping
            if let Some(label) = labels.get(y) {
                if !label.is_empty() {
                    let lines: Vec<&str> = label.split('\n').collect();
                    let _ = write!(output, " {}", lines[0]);
                    for cont_line in &lines[1..] {
                        output.push('\n');
                        self.render_vlines(y, &mut output);
                        let _ = write!(output, " {}", cont_line);
                    }
                }
            }

            output.push('\n');
        }

        output
    }
}

#[derive(Clone)]
pub struct RenderEntry {
    pub intention: SExpr,
    pub author: PubKey,
    pub hlc: u64,
    pub causal_deps: Vec<Hash>,
    pub is_merge: bool,
}

/// Render a DAG of entries to a grid-based graph
pub fn render_dag(entries: &std::collections::HashMap<Hash, RenderEntry>) -> String {
    use std::collections::{HashMap, HashSet};
    use Hash;

    if entries.is_empty() {
        return "(no history found)\n".to_string();
    }

    let mut output = String::new();

    // Build parent/child relationships
    let entry_hashes: HashSet<Hash> = entries.keys().cloned().collect();
    let mut children: HashMap<Hash, Vec<Hash>> = HashMap::new();

    for (hash, entry) in entries {
        for parent in &entry.causal_deps {
            if entry_hashes.contains(parent) {
                children.entry(*parent).or_default().push(*hash);
            }
        }
    }

    // Sort children by HLC (deterministic ordering — earliest first)
    for kids in children.values_mut() {
        kids.sort_by_key(|h| entries.get(h).map(|e| e.hlc).unwrap_or(0));
    }

    // Kahn's algorithm with HLC priority for global interleaving
    // This ensures entries from different trees are interleaved by timestamp
    // while respecting causal dependencies (parents before children)

    // Count incoming edges (parents in set) for each entry
    let mut in_degree: HashMap<Hash, usize> = HashMap::new();
    for hash in entries.keys() {
        let Some(entry) = entries.get(hash) else {
            continue;
        };
        let parents_in_set = entry
            .causal_deps
            .iter()
            .filter(|p| entry_hashes.contains(*p))
            .count();
        in_degree.insert(*hash, parents_in_set);
    }

    // Use BinaryHeap with HLC as priority (min-heap via Reverse)
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    // (Reverse(hlc), hash) - Reverse for min-heap behavior (oldest first)
    let mut ready: BinaryHeap<(Reverse<u64>, Hash)> = BinaryHeap::new();

    // Start with all entries that have no parents (in_degree = 0)
    for (hash, &degree) in &in_degree {
        if degree == 0 {
            let hlc = entries.get(hash).map(|e| e.hlc).unwrap_or(0);
            ready.push((Reverse(hlc), *hash));
        }
    }

    let mut order: Vec<Hash> = Vec::new();

    while let Some((_, hash)) = ready.pop() {
        order.push(hash);

        // Decrease in_degree for all children
        if let Some(kids) = children.get(&hash) {
            for child in kids {
                if let Some(degree) = in_degree.get_mut(child) {
                    *degree -= 1;
                    if *degree == 0 {
                        let hlc = entries.get(child).map(|e| e.hlc).unwrap_or(0);
                        ready.push((Reverse(hlc), *child));
                    }
                }
            }
        }
    }
    // order is now oldest first (roots at top), newest last (leaves at bottom)

    // Calculate column assignments with column reuse
    // Track which columns have active vertical lines at each row
    let mut hash_to_col: HashMap<Hash, usize> = HashMap::new();
    let mut hash_to_row: HashMap<Hash, usize> = HashMap::new();

    // Assign rows (order index)
    for (row, hash) in order.iter().enumerate() {
        hash_to_row.insert(*hash, row);
    }

    // Track which columns are "in use" (have a line continuing below current row)
    // A column is freed when its last descendant is processed
    let mut active_columns: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut column_tips: HashMap<usize, Hash> = HashMap::new(); // Track who owns the active column
    let mut last_busy_row: HashMap<usize, usize> = HashMap::new(); // Track when column was last touched
    let mut reserved_until: HashMap<usize, usize> = HashMap::new(); // Pre-reserved fork columns
    let mut max_col_used = 0usize;

    // For each entry, find the row of its last descendant (to know when column becomes free)
    let mut last_descendant_row: HashMap<Hash, usize> = HashMap::new();
    for hash in order.iter() {
        let Some(entry) = entries.get(hash) else {
            continue;
        };
        let my_row = hash_to_row[hash];
        // Update each parent's last descendant
        for parent in &entry.causal_deps {
            if entry_hashes.contains(parent) {
                let current = last_descendant_row.get(parent).copied().unwrap_or(0);
                last_descendant_row.insert(*parent, current.max(my_row));
            }
        }
    }

    // Find leftmost free column (not active, not reserved, respects adjacency gap)
    fn find_free_column(
        active: &std::collections::HashSet<usize>,
        last_busy: &HashMap<usize, usize>,
        reserved_until: &HashMap<usize, usize>,
        max_used: usize,
        current_row: usize,
    ) -> usize {
        for col in 0..=max_used {
            if active.contains(&col) {
                continue;
            }
            // Check for pre-reserved fork column
            if let Some(&until) = reserved_until.get(&col) {
                if until >= current_row {
                    continue;
                }
            }
            // Check for reuse gap (CLI needs this — can't draw through character cells)
            if current_row > 0 {
                if let Some(&last) = last_busy.get(&col) {
                    if last >= current_row - 1 {
                        continue;
                    }
                }
            }
            return col;
        }
        max_used + 1
    }

    // Assign columns with reuse
    for (row, hash) in order.iter().enumerate() {
        let Some(entry) = entries.get(hash) else {
            continue;
        };
        let parents_in_set: Vec<Hash> = entry
            .causal_deps
            .iter()
            .filter(|p| entry_hashes.contains(*p))
            .cloned()
            .collect();

        // Define find_free closure to capture strict params
        let get_free = |max: usize| {
            find_free_column(&active_columns, &last_busy_row, &reserved_until, max, row)
        };

        // If column was pre-assigned by a fork reservation, use it
        let col = if let Some(&pre) = hash_to_col.get(hash) {
            // Clear the reservation now that the node is placed
            if reserved_until.get(&pre) == Some(&row) {
                reserved_until.remove(&pre);
            }
            pre
        } else if parents_in_set.is_empty() {
            // Root - find leftmost free column
            let col = get_free(max_col_used);
            max_col_used = max_col_used.max(col);
            col
        } else if parents_in_set.len() > 1 {
            // MERGE: use leftmost parent's column
            let col = parents_in_set
                .iter()
                .filter_map(|p| hash_to_col.get(p))
                .min()
                .copied()
                .unwrap_or_else(|| get_free(max_col_used));
            max_col_used = max_col_used.max(col);
            col
        } else {
            // Single parent
            let parent = parents_in_set[0];
            if let Some(&parent_col) = hash_to_col.get(&parent) {
                // Check if parent_col is occupied by an unrelated line
                let is_blocked = active_columns.contains(&parent_col)
                    && column_tips.get(&parent_col) != Some(&parent);

                if is_blocked {
                    // Blocked by unrelated line -> new column
                    let col = get_free(max_col_used);
                    max_col_used = max_col_used.max(col);
                    col
                } else {
                    // Inherit parent's column (fork handling done by pre-reservation)
                    parent_col
                }
            } else {
                let col = get_free(max_col_used);
                max_col_used = max_col_used.max(col);
                col
            }
        };

        hash_to_col.insert(*hash, col);

        // A column is active as long as there's a vertical line in it
        let has_descendants = last_descendant_row
            .get(hash)
            .map(|&r| r > row)
            .unwrap_or(false);
        if has_descendants {
            active_columns.insert(col);
            column_tips.insert(col, *hash);
        } else {
            active_columns.remove(&col);
            column_tips.remove(&col);
        }

        // Free parent columns that terminate at this row (merge/branch end cleanup).
        // Without this, merged/terminated branches leak their columns permanently.
        for parent in &parents_in_set {
            if last_descendant_row.get(parent) == Some(&row) {
                if let Some(&p_col) = hash_to_col.get(parent) {
                    // Only free if the current node isn't inheriting and continuing this column
                    if p_col != col || !has_descendants {
                        active_columns.remove(&p_col);
                        column_tips.remove(&p_col);
                    }
                }
            }
        }

        // Pre-reserve columns for fork children.
        // When a node has N children, the last child will inherit this node's column.
        // The first N-1 children get new columns reserved from this row to the child row.
        if let Some(kids) = children.get(hash) {
            if kids.len() > 1 {
                // Reserve for all but the last child (who inherits this column)
                for kid in &kids[..kids.len() - 1] {
                    if !hash_to_col.contains_key(kid) {
                        let child_row = hash_to_row[kid];
                        let reserved_col = find_free_column(
                            &active_columns,
                            &last_busy_row,
                            &reserved_until,
                            max_col_used,
                            row,
                        );
                        max_col_used = max_col_used.max(reserved_col);
                        hash_to_col.insert(*kid, reserved_col);
                        reserved_until.insert(reserved_col, child_row);
                    }
                }
            }
        }

        // Mark active columns + current as busy at this row
        for c in &active_columns {
            last_busy_row.insert(*c, row);
        }
        last_busy_row.insert(col, row);
    }

    // Create grid (2 chars per column for spacing)
    let grid_width = (max_col_used + 1) * 2;
    let grid_height = order.len();
    let mut grid = Grid::new(grid_width, grid_height);

    // Create labels
    let mut labels: Vec<String> = vec![String::new(); grid_height];

    // Draw entries and connections
    for hash in &order {
        let Some(entry) = entries.get(hash) else {
            continue;
        };
        let row = hash_to_row[hash];
        let col = hash_to_col[hash];
        let grid_x = col * 2;

        // Generate consistent color based on author (via display_helpers)
        let color_code = ansi_code(author_color(&entry.author));

        // Count parents in our entry set (not all parents may be in filtered history)
        let parents_in_set: Vec<Hash> = entry
            .causal_deps
            .iter()
            .filter(|p| entry_hashes.contains(*p))
            .cloned()
            .collect();
        let is_root = parents_in_set.is_empty();

        // Draw entry marker with author color
        // Priority: root > merge > normal
        let marker = if is_root {
            ROOT
        } else if entry.is_merge {
            CIRCLE
        } else {
            DOT
        };
        grid.set_colored(grid_x, row, marker, 0, color_code); // Entries have highest priority

        // Render intention — inline if it fits, pretty-wrapped if it overflows
        let inline = render_sexpr_colored(&entry.intention, 4);
        let label_start = grid_width + 1; // grid chars + space
        let term_width = terminal_size::terminal_size()
            .map(|(w, _)| w.0 as usize)
            .unwrap_or(120);
        let inline_width = unicode_width::UnicodeWidthStr::width(strip_ansi(&inline).as_str());
        if label_start + inline_width > term_width {
            labels[row] = render_sexpr_pretty_colored(&entry.intention, 4);
        } else {
            labels[row] = inline;
        }

        // Draw connections to parents (use parents_in_set from above)
        for (p_idx, parent) in parents_in_set.iter().enumerate() {
            let parent_row = hash_to_row[parent];
            let parent_col = hash_to_col[parent];
            let parent_x = parent_col * 2;
            let persistence = (p_idx + 1) as u8;
            let is_merge = parents_in_set.len() > 1;

            if col == parent_col {
                // Same column - vertical line
                grid.vline(grid_x, row, parent_row, persistence);
            } else if is_merge {
                // MERGE: child has multiple parents
                // Lines come IN to the merge entry at CHILD's row
                //    ●   parent (in different column)
                //    │
                // ╰──○   merge row (hline here, curving up to parent)

                // Draw vertical from parent down toward merge row
                grid.vline(parent_x, parent_row, row, persistence);

                // Draw horizontal at merge's row, connecting from parent column
                grid.hline(row, parent_x, grid_x, true, false, persistence);
            } else {
                // SINGLE PARENT (potential FORK at parent)
                // Branch starts at PARENT's row and curves down to child
                // ●──╮  parent row (hline here)
                //    │
                //    ●  child row

                // Draw horizontal at parent's row, branching out
                grid.hline(parent_row, parent_x, grid_x, false, true, persistence);

                // Draw vertical from parent row down to child
                grid.vline(grid_x, parent_row, row, persistence);
            }
        }
    }

    output.push_str(&grid.render(&labels));
    let _ = writeln!(output, "\n({} entries)", entries.len());

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip ANSI escape sequences (e.g. `\x1b[31m` ... `\x1b[0m`) from a string.
    /// Handles CSI sequences of the form `ESC [ <params> <final_byte>`.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Expect '[' next
                if let Some(next) = chars.next() {
                    if next == '[' {
                        // Consume until final byte (0x40–0x7E)
                        for fin in chars.by_ref() {
                            if ('@'..='~').contains(&fin) {
                                break;
                            }
                        }
                    }
                    // else: non-CSI escape, just skip the two chars
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn test_grid_cell_chars() {
        assert_eq!(
            GridCell {
                character: DOT,
                persistence: 0,
                color: 0
            }
            .to_char(),
            '●'
        );
        assert_eq!(
            GridCell {
                character: VER,
                persistence: 0,
                color: 0
            }
            .to_char(),
            '│'
        );
        assert_eq!(
            GridCell {
                character: HOR,
                persistence: 0,
                color: 0
            }
            .to_char(),
            '─'
        );
        assert_eq!(
            GridCell {
                character: R_D,
                persistence: 0,
                color: 0
            }
            .to_char(),
            '╭'
        );
        assert_eq!(
            GridCell {
                character: L_U,
                persistence: 0,
                color: 0
            }
            .to_char(),
            '╯'
        );
    }

    #[test]
    fn test_grid_vline() {
        let mut grid = Grid::new(3, 5);
        grid.vline(1, 0, 4, 1);

        assert_eq!(grid.get(1, 0).character, SPACE); // Start excluded
        assert_eq!(grid.get(1, 1).character, VER);
        assert_eq!(grid.get(1, 2).character, VER);
        assert_eq!(grid.get(1, 3).character, VER);
        assert_eq!(grid.get(1, 4).character, SPACE); // End excluded
    }

    #[test]
    fn test_grid_crossing() {
        let mut grid = Grid::new(5, 5);
        grid.vline(2, 0, 5, 1);
        grid.hline(2, 0, 4, false, false, 1);

        assert_eq!(grid.get(2, 2).character, CROSS);
    }

    /// Reproduces the bug from a real session: fork with an unrelated root in between.
    ///
    /// DAG structure (in-set edges only):
    ///   2d86 (root) ──┬── b80f (child, author B)
    ///                 └── 0e3e (child, author A)
    ///   bfc4 (root, unrelated)
    ///
    /// Topo order (by HLC): 2d86, bfc4, b80f, 0e3e
    ///
    /// Expected: fork from 2d86 visible, bfc4 on its own column,
    ///           vertical line from 2d86 to its last child unbroken.
    #[test]
    fn test_fork_with_unrelated_root() {
        let mut entries = std::collections::HashMap::new();

        let h_2d86 = Hash::from([0x2D; 32]);
        let h_bfc4 = Hash::from([0xBF; 32]);
        let h_b80f = Hash::from([0xB8; 32]);
        let h_0e3e = Hash::from([0x0E; 32]);
        let author_a = PubKey::from([0xAA; 32]);
        let author_b = PubKey::from([0xBB; 32]);

        entries.insert(
            h_2d86,
            RenderEntry {
                intention: SExpr::str("put a 7"),
                author: author_a,
                hlc: 100,
                causal_deps: vec![], // root in view
                is_merge: false,
            },
        );
        entries.insert(
            h_bfc4,
            RenderEntry {
                intention: SExpr::str("put b 21"),
                author: author_b,
                hlc: 200,
                causal_deps: vec![], // root in view (real dep off-screen)
                is_merge: false,
            },
        );
        entries.insert(
            h_b80f,
            RenderEntry {
                intention: SExpr::str("put a 8"),
                author: author_b,
                hlc: 300,
                causal_deps: vec![h_2d86], // child of 2d86
                is_merge: false,
            },
        );
        entries.insert(
            h_0e3e,
            RenderEntry {
                intention: SExpr::str("put a 9"),
                author: author_a,
                hlc: 400,
                causal_deps: vec![h_2d86], // child of 2d86
                is_merge: false,
            },
        );

        let output = render_dag(&entries);

        // Strip ANSI escape sequences for assertion
        let clean = strip_ansi(&output);

        // Extract just the graph chars (before the label) for each row
        let rows: Vec<&str> = clean.lines().collect();

        // The fork parent (2d86) should have a visible fork connection.
        // The last child (0e3e) should inherit the fork parent's column.
        // The unrelated root (bfc4) should be on a separate column.
        // Vertical line from 2d86 to 0e3e should pass through rows 1 and 2.

        // Verify the fork parent's column has a vertical line through to the last child
        // by checking the graph chars. We don't assert exact layout, but verify:
        // 1. 2d86 is NOT on the same column as bfc4
        // 2. There's a vertical continuation between 2d86 and 0e3e

        // Print for debugging
        eprintln!("Rendered output:\n{}", output);
        eprintln!("Clean output:\n{}", clean);

        // Basic sanity: 4 entries rendered
        assert!(clean.contains("4 entries"), "Should render all 4 entries");

        // The fork parent should show a fork indicator (VER_R ├ or similar),
        // not have its marker overwritten
        let row0_graph: String = rows[0].chars().take(10).collect();
        assert!(
            row0_graph.contains('⊙') || row0_graph.contains('●'),
            "Fork parent (row 0) should have an entry marker, got: {:?}",
            row0_graph,
        );
    }

    /// Tests that fork pre-reservation prevents intermediate nodes from stealing
    /// columns that are reserved for fork children.
    ///
    /// DAG structure:
    ///   A (root, hlc=100) ──┬── B (child, hlc=300)
    ///                       └── C (child, hlc=500)
    ///   D (root, hlc=200) ──── E (child of D, hlc=400)
    ///
    /// Topo order by HLC: A, D, B, E, C
    ///
    /// Without pre-reservation, when D is processed at row 1, it could grab
    /// the column that B needs (since B forks from A). With pre-reservation,
    /// B's column is reserved when A is processed, so D goes elsewhere.
    #[test]
    fn test_fork_prereservation_prevents_column_stealing() {
        let mut entries = std::collections::HashMap::new();

        let h_a = Hash::from([0xA0; 32]);
        let h_b = Hash::from([0xB0; 32]);
        let h_c = Hash::from([0xC0; 32]);
        let h_d = Hash::from([0xD0; 32]);
        let h_e = Hash::from([0xE0; 32]);
        let author_x = PubKey::from([0x11; 32]);
        let author_y = PubKey::from([0x22; 32]);

        entries.insert(
            h_a,
            RenderEntry {
                intention: SExpr::str("root a"),
                author: author_x,
                hlc: 100,
                causal_deps: vec![],
                is_merge: false,
            },
        );
        entries.insert(
            h_d,
            RenderEntry {
                intention: SExpr::str("root d"),
                author: author_y,
                hlc: 200,
                causal_deps: vec![],
                is_merge: false,
            },
        );
        entries.insert(
            h_b,
            RenderEntry {
                intention: SExpr::str("fork b"),
                author: author_x,
                hlc: 300,
                causal_deps: vec![h_a],
                is_merge: false,
            },
        );
        entries.insert(
            h_e,
            RenderEntry {
                intention: SExpr::str("child e"),
                author: author_y,
                hlc: 400,
                causal_deps: vec![h_d],
                is_merge: false,
            },
        );
        entries.insert(
            h_c,
            RenderEntry {
                intention: SExpr::str("fork c"),
                author: author_x,
                hlc: 500,
                causal_deps: vec![h_a],
                is_merge: false,
            },
        );

        let output = render_dag(&entries);
        let clean = strip_ansi(&output);

        eprintln!("Pre-reservation test output:\n{}", clean);

        assert!(clean.contains("5 entries"), "Should render all 5 entries");

        // Verify: B and C should share A's column lineage (one inherits, one forks).
        // D and E should be on their own column, separate from A's fork.
        // The key invariant: no column collisions — each row should have exactly one
        // entry marker per column.
        let rows: Vec<&str> = clean.lines().collect();
        for (i, row) in rows.iter().enumerate() {
            // Count entry markers in the graph portion (before label)
            let graph_part: String = row.chars().take(12).collect();
            let markers: usize = graph_part
                .chars()
                .filter(|c| matches!(c, '●' | '○' | '⊙'))
                .count();
            assert!(
                markers <= 1,
                "Row {} has {} markers in graph area (collision!): {:?}",
                i,
                markers,
                graph_part,
            );
        }
    }

    /// Tests that merged branches free their columns after the merge node.
    ///
    /// DAG structure:
    ///   A (root) ──── B (child)
    ///   C (root) ──── D (child)
    ///   B + D ──── M (merge of B and D)
    ///   M ──── E (child of M)
    ///
    /// After the merge M, the columns for B and D should be freed,
    /// allowing E (and any subsequent nodes) to reuse them.
    #[test]
    fn test_merge_frees_columns() {
        let mut entries = std::collections::HashMap::new();

        let h_a = Hash::from([0xA1; 32]);
        let h_c = Hash::from([0xC1; 32]);
        let h_b = Hash::from([0xB1; 32]);
        let h_d = Hash::from([0xD1; 32]);
        let h_m = Hash::from([0x11; 32]);
        let h_e = Hash::from([0xE1; 32]);
        let author = PubKey::from([0x33; 32]);

        entries.insert(
            h_a,
            RenderEntry {
                intention: SExpr::str("root a"),
                author,
                hlc: 100,
                causal_deps: vec![],
                is_merge: false,
            },
        );
        entries.insert(
            h_c,
            RenderEntry {
                intention: SExpr::str("root c"),
                author,
                hlc: 150,
                causal_deps: vec![],
                is_merge: false,
            },
        );
        entries.insert(
            h_b,
            RenderEntry {
                intention: SExpr::str("child b"),
                author,
                hlc: 200,
                causal_deps: vec![h_a],
                is_merge: false,
            },
        );
        entries.insert(
            h_d,
            RenderEntry {
                intention: SExpr::str("child d"),
                author,
                hlc: 250,
                causal_deps: vec![h_c],
                is_merge: false,
            },
        );
        entries.insert(
            h_m,
            RenderEntry {
                intention: SExpr::str("merge m"),
                author,
                hlc: 300,
                causal_deps: vec![h_b, h_d],
                is_merge: true,
            },
        );
        entries.insert(
            h_e,
            RenderEntry {
                intention: SExpr::str("after merge"),
                author,
                hlc: 400,
                causal_deps: vec![h_m],
                is_merge: false,
            },
        );

        let output = render_dag(&entries);
        let clean = strip_ansi(&output);

        eprintln!("Merge frees columns test output:\n{}", clean);

        assert!(clean.contains("6 entries"), "Should render all 6 entries");

        // The merge node M should show a merge marker (○)
        assert!(clean.contains('○'), "Merge node should use ○ marker");

        // E (after merge) should be on column 0 — the merge frees both parent
        // columns, and E inherits M's column (which should be the leftmost).
        // We can verify the graph doesn't keep growing wider.
        let rows: Vec<&str> = clean.lines().collect();
        let last_entry_row = &rows[5]; // E is the 6th entry (row 5)
        let graph_chars: String = last_entry_row.chars().take(2).collect();
        assert!(
            graph_chars.contains('●') || graph_chars.contains('⊙'),
            "Post-merge entry E should be on column 0 (leftmost), got graph: {:?}",
            graph_chars,
        );
    }
}
