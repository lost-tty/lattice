//! Grid-based graph renderer for history visualization.
//! Uses box drawing characters and z-ordering for crossovers.
//!
//! Design inspired by git-graph: https://github.com/mlange-42/git-graph (MIT)

use std::fmt::Write;
use lattice_core::PubKey;

// Character codes for grid cells
pub const SPACE: u8 = 0;
pub const DOT: u8 = 1;      // ● entry marker
pub const CIRCLE: u8 = 2;   // ○ merge entry marker
pub const ROOT: u8 = 3;     // ⊙ root entry marker (no parents)
pub const TOMBSTONE: u8 = 4; // ✖ delete entry marker
pub const VER: u8 = 5;      // │ vertical line
pub const HOR: u8 = 6;      // ─ horizontal line
pub const CROSS: u8 = 7;    // ┼ crossing
pub const R_U: u8 = 8;      // ╰ right-up corner
pub const R_D: u8 = 9;      // ╭ right-down corner
pub const L_D: u8 = 10;     // ╮ left-down corner
pub const L_U: u8 = 11;     // ╯ left-up corner
pub const VER_L: u8 = 12;   // ┤ vertical with left branch
pub const VER_R: u8 = 13;   // ├ vertical with right branch
pub const HOR_U: u8 = 14;   // ┴ horizontal with up branch
pub const HOR_D: u8 = 15;   // ┬ horizontal with down branch
pub const ARR_L: u8 = 16;   // ⟨ left angle bracket (merge indicator)
pub const ARR_R: u8 = 17;   // ⟩ right angle bracket (merge indicator)

// Character mappings
const CHARS: [char; 18] = [
    ' ',  // SPACE
    '●',  // DOT
    '○',  // CIRCLE
    '⊙',  // ROOT
    '✖',  // TOMBSTONE
    '│',  // VER
    '─',  // HOR
    '┼',  // CROSS
    '╰',  // R_U
    '╭',  // R_D
    '╮',  // L_D
    '╯',  // L_U
    '┤',  // VER_L
    '├',  // VER_R
    '┴',  // HOR_U
    '┬',  // HOR_D
    '⟨',  // ARR_L
    '⟩',  // ARR_R
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
            
            (HOR, L_U) | (HOR, R_U) => HOR_U,
            (HOR, L_D) | (HOR, R_D) => HOR_D,
            (L_U, HOR) | (R_U, HOR) => HOR_U,
            (L_D, HOR) | (R_D, HOR) => HOR_D,
            
            // T-junctions meeting lines
            (VER_L, HOR) | (VER_R, HOR) => CROSS,
            (HOR_U, VER) | (HOR_D, VER) => CROSS,
            (HOR, VER_L) | (HOR, VER_R) => CROSS,
            (VER, HOR_U) | (VER, HOR_D) => CROSS,
            
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
    pub fn hline(&mut self, y: usize, x1: usize, x2: usize, is_merge: bool, curves_down: bool, persistence: u8) {
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
                self.set_with_merge(left, y, VER_R, persistence);  // ├ branch point
                self.set_with_merge(right, y, L_D, persistence);   // ╮ curve down
            } else {
                self.set_with_merge(right, y, VER_L, persistence); // ┤ branch point
                self.set_with_merge(left, y, R_D, persistence);    // ╭ curve down
            }
        } else {
            // Merging: horizontal then up
            // ╰──●  (R_U at far end to curve up)
            if x1 < x2 {
                self.set_with_merge(left, y, R_U, persistence);    // ╰ curve up
                self.set_with_merge(right, y, VER_L, persistence); // ┤ merge point
            } else {
                self.set_with_merge(right, y, L_U, persistence);   // ╯ curve up
                self.set_with_merge(left, y, VER_R, persistence);  // ├ merge point
            }
        }
    }
    
    /// Render grid to string, appending text labels on the right
    pub fn render(&self, labels: &[String]) -> String {
        let mut output = String::new();
        
        for y in 0..self.height {
            // Render graph columns
            for x in 0..self.width {
                let cell = self.get(x, y);
                if cell.color > 0 {
                    // Output with color
                    write!(output, "\x1b[{}m{}\x1b[0m", cell.color, cell.to_char()).unwrap();
                } else {
                    output.push(cell.to_char());
                }
            }
            
            // Append label if present
            if let Some(label) = labels.get(y) {
                if !label.is_empty() {
                    write!(output, " {}", label).unwrap();
                }
            }
            
            output.push('\n');
        }
        
        output
    }
}

/// Entry info for history rendering
#[derive(Clone)]
pub struct RenderEntry {
    pub key: Vec<u8>,
    pub author: PubKey,
    pub hlc: u64,
    pub value: Vec<u8>,
    pub tombstone: bool,
    pub parent_hashes: Vec<lattice_core::Hash>,
    pub is_merge: bool,
}

/// Render a DAG of entries to a grid-based graph
pub fn render_dag(
    entries: &std::collections::HashMap<lattice_core::Hash, RenderEntry>,
    _key: &[u8],
) -> String {
    use std::collections::{HashMap, HashSet};
    use lattice_core::Hash;
    
    if entries.is_empty() {
        return "(no history found)\n".to_string();
    }
    
    let mut output = String::new();
    
    // Build parent/child relationships
    let entry_hashes: HashSet<Hash> = entries.keys().cloned().collect();
    let mut children: HashMap<Hash, Vec<Hash>> = HashMap::new();
    
    for (hash, entry) in entries {
        for parent in &entry.parent_hashes {
            if entry_hashes.contains(parent) {
                children.entry(*parent).or_default().push(*hash);
            }
        }
    }
    
    // Kahn's algorithm with HLC priority for global interleaving
    // This ensures entries from different trees are interleaved by timestamp
    // while respecting causal dependencies (parents before children)
    
    // Count incoming edges (parents in set) for each entry
    let mut in_degree: HashMap<Hash, usize> = HashMap::new();
    for hash in entries.keys() {
        let parents_in_set = entries.get(hash).unwrap().parent_hashes.iter()
            .filter(|p| entry_hashes.contains(*p))
            .count();
        in_degree.insert(*hash, parents_in_set);
    }
    
    // Use BinaryHeap with HLC as priority (min-heap via Reverse)
    use std::collections::BinaryHeap;
    use std::cmp::Reverse;
    
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
    let mut max_col_used = 0usize;
    
    // For each entry, find the row of its last descendant (to know when column becomes free)
    let mut last_descendant_row: HashMap<Hash, usize> = HashMap::new();
    for hash in order.iter() {
        let entry = entries.get(hash).unwrap();
        let my_row = hash_to_row[hash];
        // Update each parent's last descendant
        for parent in &entry.parent_hashes {
            if entry_hashes.contains(parent) {
                let current = last_descendant_row.get(parent).copied().unwrap_or(0);
                last_descendant_row.insert(*parent, current.max(my_row));
            }
        }
    }
    
    // Find leftmost free column
    fn find_free_column(active: &std::collections::HashSet<usize>, max_used: usize) -> usize {
        for col in 0..=max_used {
            if !active.contains(&col) {
                return col;
            }
        }
        max_used + 1
    }
    
    // Assign columns with reuse
    for (row, hash) in order.iter().enumerate() {
        let entry = entries.get(hash).unwrap();
        let parents_in_set: Vec<Hash> = entry.parent_hashes.iter()
            .filter(|p| entry_hashes.contains(*p))
            .cloned()
            .collect();
        
        let col = if parents_in_set.is_empty() {
            // Root - find leftmost free column
            let col = find_free_column(&active_columns, max_col_used);
            max_col_used = max_col_used.max(col);
            col
        } else if parents_in_set.len() > 1 {
            // MERGE: use leftmost parent's column
            parents_in_set.iter()
                .filter_map(|p| hash_to_col.get(p))
                .min()
                .copied()
                .unwrap_or(0)
        } else {
            // Single parent
            let parent = parents_in_set[0];
            if let Some(&parent_col) = hash_to_col.get(&parent) {
                let siblings = children.get(&parent).map(|s| s.len()).unwrap_or(0);
                if siblings <= 1 {
                    parent_col
                } else {
                    // Fork - first sibling inherits, others get new column
                    let first_sibling = children.get(&parent).and_then(|s| s.first());
                    if first_sibling == Some(hash) {
                        parent_col
                    } else {
                        let col = find_free_column(&active_columns, max_col_used);
                        max_col_used = max_col_used.max(col);
                        col
                    }
                }
            } else {
                let col = find_free_column(&active_columns, max_col_used);
                max_col_used = max_col_used.max(col);
                col
            }
        };
        
        hash_to_col.insert(*hash, col);
        
        // A column is active as long as there's a vertical line in it
        // This happens when an entry has descendants (children or merge children)
        // The column should stay active until the LAST row that needs it
        let has_descendants = last_descendant_row.get(hash).map(|&r| r > row).unwrap_or(false);
        if has_descendants {
            active_columns.insert(col);
        } else {
            // This entry has no more descendants - free its column
            // But only if we're the one who made it active
            active_columns.remove(&col);
        }
        
        // Also need to keep parent columns active until this row if parent is in different column
        // (the vertical line from parent to here blocks that column)
        // This is already handled by parents not being freed until their last_descendant_row
    }
    
    // Create grid (2 chars per column for spacing)
    let grid_width = (max_col_used + 1) * 2;
    let grid_height = order.len();
    let mut grid = Grid::new(grid_width, grid_height);
    
    // Create labels
    let mut labels: Vec<String> = vec![String::new(); grid_height];
    
    // Draw entries and connections
    for hash in &order {
        let entry = entries.get(hash).unwrap();
        let row = hash_to_row[hash];
        let col = hash_to_col[hash];
        let grid_x = col * 2;
        
        // Generate color based on author hash (use first bytes to pick from 6 colors)
        // Avoid black (30) and white (37) for better visibility
        let color_code = 31 + (entry.author[0] % 6); // 31-36: red, green, yellow, blue, magenta, cyan
        
        // Count parents in our entry set (not all parents may be in filtered history)
        let parents_in_set: Vec<Hash> = entry.parent_hashes.iter()
            .filter(|p| entry_hashes.contains(*p))
            .cloned()
            .collect();
        let is_root = parents_in_set.is_empty();
        
        // Draw entry marker with author color
        // Priority: tombstone > root > merge > normal
        let marker = if entry.tombstone { 
            TOMBSTONE 
        } else if is_root { 
            ROOT 
        } else if entry.is_merge { 
            CIRCLE 
        } else { 
            DOT 
        };
        grid.set_colored(grid_x, row, marker, 0, color_code); // Entries have highest priority
        
        // Create colored label
        let hash_short = hex::encode(&hash[..4]);
        let author_short = hex::encode(&entry.author[..4]);
        let key_display = String::from_utf8_lossy(&entry.key);
        let val_str = if entry.tombstone { 
            "⊗".to_string() 
        } else { 
            String::from_utf8_lossy(&entry.value).to_string()
        };
        
        // Format with ANSI color: \x1b[{color}m ... \x1b[0m
        let marker_char = if entry.tombstone { 
            '✖' 
        } else if is_root { 
            '⊙' 
        } else if entry.is_merge { 
            '○' 
        } else { 
            '●' 
        };
        labels[row] = format!(
            "\x1b[{}m{}\x1b[0m [{}] {}={} \x1b[{}m(a:{})\x1b[0m", 
            color_code, marker_char, hash_short, key_display, val_str, color_code, author_short
        );
        
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
    
    #[test]
    fn test_grid_cell_chars() {
        assert_eq!(GridCell { character: DOT, persistence: 0, color: 0 }.to_char(), '●');
        assert_eq!(GridCell { character: VER, persistence: 0, color: 0 }.to_char(), '│');
        assert_eq!(GridCell { character: HOR, persistence: 0, color: 0 }.to_char(), '─');
        assert_eq!(GridCell { character: R_D, persistence: 0, color: 0 }.to_char(), '╭');
        assert_eq!(GridCell { character: L_U, persistence: 0, color: 0 }.to_char(), '╯');
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
}
