//! Grid-based graph renderer for history visualization.
//! Uses box drawing characters and z-ordering for crossovers.
//!
//! Design inspired by git-graph: https://github.com/mlange-42/git-graph (MIT)

use std::fmt::Write;

// Character codes for grid cells
pub const SPACE: u8 = 0;
pub const DOT: u8 = 1;      // ● entry marker
pub const CIRCLE: u8 = 2;   // ○ merge entry marker
pub const VER: u8 = 3;      // │ vertical line
pub const HOR: u8 = 4;      // ─ horizontal line
pub const CROSS: u8 = 5;    // ┼ crossing
pub const R_U: u8 = 6;      // ╰ right-up corner
pub const R_D: u8 = 7;      // ╭ right-down corner
pub const L_D: u8 = 8;      // ╮ left-down corner
pub const L_U: u8 = 9;      // ╯ left-up corner
pub const VER_L: u8 = 10;   // ┤ vertical with left branch
pub const VER_R: u8 = 11;   // ├ vertical with right branch
pub const HOR_U: u8 = 12;   // ┴ horizontal with up branch
pub const HOR_D: u8 = 13;   // ┬ horizontal with down branch
pub const ARR_L: u8 = 14;   // ⟨ left angle bracket (merge indicator)
pub const ARR_R: u8 = 15;   // ⟩ right angle bracket (merge indicator)

// Character mappings
const CHARS: [char; 16] = [
    ' ',  // SPACE
    '●',  // DOT
    '○',  // CIRCLE
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
    pub author: [u8; 32],
    pub hlc: u64,
    pub value: Vec<u8>,
    pub tombstone: bool,
    pub parent_hashes: Vec<[u8; 32]>,
    pub is_merge: bool,
}

/// Render a DAG of entries to a grid-based graph
pub fn render_dag(
    entries: &std::collections::HashMap<[u8; 32], RenderEntry>,
    key: &[u8],
) -> String {
    use std::collections::{HashMap, HashSet};
    
    if entries.is_empty() {
        return "(no history found)\n".to_string();
    }
    
    let mut output = String::new();
    let key_str = String::from_utf8_lossy(key);
    
    // Build parent/child relationships
    let entry_hashes: HashSet<[u8; 32]> = entries.keys().cloned().collect();
    let mut children: HashMap<[u8; 32], Vec<[u8; 32]>> = HashMap::new();
    
    for (hash, entry) in entries {
        for parent in &entry.parent_hashes {
            if entry_hashes.contains(parent) {
                children.entry(*parent).or_default().push(*hash);
            }
        }
    }
    
    // Find roots (no parents in set)
    let mut roots: Vec<[u8; 32]> = entries.iter()
        .filter(|(_, e)| e.parent_hashes.is_empty() || 
                !e.parent_hashes.iter().any(|p| entry_hashes.contains(p)))
        .map(|(h, _)| *h)
        .collect();
    roots.sort_by_key(|h| entries.get(h).map(|e| e.hlc).unwrap_or(0));
    
    // Sort children by HLC descending (newest first)
    for kids in children.values_mut() {
        kids.sort_by_key(|h| std::cmp::Reverse(entries.get(h).map(|e| e.hlc).unwrap_or(0)));
    }
    
    // Topological sort (leaves first)
    let mut visited = HashSet::new();
    let mut order: Vec<[u8; 32]> = Vec::new();
    
    fn visit(hash: [u8; 32], children: &HashMap<[u8; 32], Vec<[u8; 32]>>, 
             visited: &mut HashSet<[u8; 32]>, order: &mut Vec<[u8; 32]>) {
        if visited.contains(&hash) { return; }
        visited.insert(hash);
        if let Some(kids) = children.get(&hash) {
            for child in kids {
                visit(*child, children, visited, order);
            }
        }
        order.push(hash);
    }
    
    for root in &roots {
        visit(*root, &children, &mut visited, &mut order);
    }
    // order is now leaves first, roots last - reverse so oldest (roots) at top, newest at bottom
    order.reverse();
    
    // Calculate column assignments using branch tracing
    // Each entry needs a column; forks create new columns to the right
    let mut hash_to_col: HashMap<[u8; 32], usize> = HashMap::new();
    let mut hash_to_row: HashMap<[u8; 32], usize> = HashMap::new();
    let mut max_col = 0usize;
    
    // Assign rows (order index)
    for (row, hash) in order.iter().enumerate() {
        hash_to_row.insert(*hash, row);
    }
    
    // Assign columns by tracing from roots (oldest) to leaves (newest)
    // First child inherits parent column, additional children get new columns (fork right)
    for hash in order.iter() {
        let entry = entries.get(hash).unwrap();
        let parents: Vec<[u8; 32]> = entry.parent_hashes.iter()
            .filter(|p| entry_hashes.contains(*p))
            .cloned()
            .collect();
        
        if parents.is_empty() {
            // Root - assign new column
            hash_to_col.insert(*hash, max_col);
            max_col += 1;
        } else if parents.len() > 1 {
            // MERGE: multiple parents - use the leftmost parent's column
            // This keeps the graph compact after merges
            let min_col = parents.iter()
                .filter_map(|p| hash_to_col.get(p))
                .min()
                .copied()
                .unwrap_or(0);
            hash_to_col.insert(*hash, min_col);
        } else {
            // Single parent - check if we're first child
            let parent = parents[0];
            if let Some(&parent_col) = hash_to_col.get(&parent) {
                let siblings = children.get(&parent).map(|s| s.len()).unwrap_or(0);
                if siblings <= 1 {
                    hash_to_col.insert(*hash, parent_col);
                } else {
                    // Fork - first sibling inherits, others branch right
                    let first_sibling = children.get(&parent).and_then(|s| s.first());
                    if first_sibling == Some(hash) {
                        hash_to_col.insert(*hash, parent_col);
                    } else {
                        hash_to_col.insert(*hash, max_col);
                        max_col += 1;
                    }
                }
            } else {
                hash_to_col.insert(*hash, max_col);
                max_col += 1;
            }
        }
    }
    
    // Create grid (2 chars per column for spacing)
    let grid_width = (max_col + 1) * 2;
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
        
        // Draw entry marker with author color
        let marker = if entry.is_merge { CIRCLE } else { DOT };
        grid.set_colored(grid_x, row, marker, 0, color_code); // Entries have highest priority
        
        // Create colored label
        let hash_short = hex::encode(&hash[..4]);
        let author_short = hex::encode(&entry.author[..4]);
        let val_str = if entry.tombstone { 
            "⊗".to_string() 
        } else { 
            String::from_utf8_lossy(&entry.value).to_string()
        };
        
        // Format with ANSI color: \x1b[{color}m ... \x1b[0m
        let marker_char = if entry.is_merge { '○' } else { '●' };
        labels[row] = format!(
            "\x1b[{}m{}\x1b[0m [{}] {}={} \x1b[{}m(a:{})\x1b[0m", 
            color_code, marker_char, hash_short, key_str, val_str, color_code, author_short
        );
        
        // Draw connections to parents
        let parents: Vec<[u8; 32]> = entry.parent_hashes.iter()
            .filter(|p| entry_hashes.contains(*p))
            .cloned()
            .collect();
        
        for (p_idx, parent) in parents.iter().enumerate() {
            let parent_row = hash_to_row[parent];
            let parent_col = hash_to_col[parent];
            let parent_x = parent_col * 2;
            let persistence = (p_idx + 1) as u8;
            let is_merge = parents.len() > 1;
            
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
        assert_eq!(GridCell::new(DOT, 0).to_char(), '●');
        assert_eq!(GridCell::new(VER, 0).to_char(), '│');
        assert_eq!(GridCell::new(HOR, 0).to_char(), '─');
        assert_eq!(GridCell::new(R_D, 0).to_char(), '╭');
        assert_eq!(GridCell::new(L_U, 0).to_char(), '╯');
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
        grid.hline(2, 0, 4, false, 1);
        
        assert_eq!(grid.get(2, 2).character, CROSS);
    }
}
