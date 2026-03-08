use super::{NodeAuthor, NodeStatus, Poset};

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[96m";
const GREEN: &str = "\x1b[92m";
const YELLOW: &str = "\x1b[93m";
const RED: &str = "\x1b[91m";
const MAGENTA: &str = "\x1b[95m";
const BLUE: &str = "\x1b[94m";

// Suppress unused-constant warning for YELLOW (used as status color future use)
#[allow(dead_code)]
const _YELLOW_USED: &str = YELLOW;

struct Cell {
    ch: char,
    color: &'static str,
    depth: f32,
}

impl Default for Cell {
    fn default() -> Self { Self { ch: ' ', color: "", depth: f32::NEG_INFINITY } }
}

/// Render the poset into exactly `height` terminal lines of width `width`.
pub fn render(poset: &Poset, width: usize, height: usize) -> Vec<String> {
    if poset.nodes.is_empty() || width == 0 || height == 0 {
        return Vec::new();
    }

    let mut grid: Vec<Vec<Cell>> = (0..height)
        .map(|_| (0..width).map(|_| Cell::default()).collect())
        .collect();

    let cx = width as f32 / 2.0;
    let cy = height as f32 / 2.0;
    let scale_x = width as f32 / 9.0;
    let scale_y = scale_x * 0.45;

    struct Proj { id: usize, sx: f32, sy: f32, depth: f32 }

    let projections: Vec<Proj> = poset.nodes.iter().map(|node| {
        let p = project(node.pos, poset.yaw, poset.pitch);
        Proj { id: node.id, sx: cx + p[0] * scale_x, sy: cy - p[1] * scale_y, depth: p[2] }
    }).collect();

    // Edges first
    for &(before, after) in &poset.edges {
        if let (Some(p1), Some(p2)) = (
            projections.iter().find(|p| p.id == before),
            projections.iter().find(|p| p.id == after),
        ) {
            draw_edge(&mut grid, [p1.sx, p1.sy], [p2.sx, p2.sy],
                      (p1.depth + p2.depth) / 2.0, width, height);
        }
    }

    // Nodes back-to-front
    let mut order: Vec<(&super::Node, &Proj)> = poset.nodes.iter().zip(projections.iter()).collect();
    order.sort_by(|(_, a), (_, b)| a.depth.partial_cmp(&b.depth).unwrap_or(std::cmp::Ordering::Equal));

    for (node, proj) in &order {
        let (ch, color) = node_glyph(node, proj.depth);
        put(&mut grid, proj.sx as isize, proj.sy as isize, ch, color, proj.depth, width, height);
        let label = clip(&node.label, 14);
        for (i, c) in label.chars().enumerate() {
            put(&mut grid, proj.sx as isize + 2 + i as isize, proj.sy as isize,
                c, color, proj.depth - 0.05, width, height);
        }
    }

    grid.into_iter().map(|row| {
        let mut s = String::new();
        let mut cur: &str = "";
        for cell in row {
            if cell.color != cur {
                if !cur.is_empty() { s.push_str(RESET); }
                if !cell.color.is_empty() { s.push_str(cell.color); }
                cur = cell.color;
            }
            s.push(cell.ch);
        }
        if !cur.is_empty() { s.push_str(RESET); }
        s
    }).collect()
}

fn project(pos: [f32; 3], yaw: f32, pitch: f32) -> [f32; 3] {
    let (sy, cy) = yaw.sin_cos();
    let x1 = pos[0] * cy + pos[2] * sy;
    let z1 = -pos[0] * sy + pos[2] * cy;
    let (sp, cp) = pitch.sin_cos();
    [x1, pos[1] * cp - z1 * sp, pos[1] * sp + z1 * cp]
}

fn node_glyph(node: &super::Node, depth: f32) -> (char, &'static str) {
    let color: &'static str = match node.status {
        NodeStatus::Done => GREEN,
        NodeStatus::Running => CYAN,
        NodeStatus::Failed => RED,
        NodeStatus::Pending => match node.author {
            NodeAuthor::User => MAGENTA,
            NodeAuthor::Ai => BLUE,
        },
    };
    let ch = node.kind.symbol(depth > -0.2);
    (ch, color)
}

fn draw_edge(grid: &mut Vec<Vec<Cell>>, p1: [f32; 2], p2: [f32; 2],
             depth: f32, width: usize, height: usize) {
    let dx = p2[0] - p1[0];
    let dy = p2[1] - p1[1];
    let steps = ((dx.abs() + dy.abs()) as usize).max(1);
    let a = dy.atan2(dx).abs();
    let frac_pi_8 = std::f32::consts::FRAC_PI_4 / 2.0;
    let ch = if a < frac_pi_8 || a > 7.0 * frac_pi_8 { '─' }
             else if (a - std::f32::consts::FRAC_PI_2).abs() < frac_pi_8 { '│' }
             else if a < std::f32::consts::FRAC_PI_2 { '╱' }
             else { '╲' };
    for i in 1..steps {
        let t = i as f32 / steps as f32;
        let x = (p1[0] + dx * t) as isize;
        let y = (p1[1] + dy * t) as isize;
        if x >= 0 && x < width as isize && y >= 0 && y < height as isize {
            let (x, y) = (x as usize, y as usize);
            if grid[y][x].ch == ' ' {
                grid[y][x] = Cell { ch, color: DIM, depth: depth - 0.5 };
            }
        }
    }
}

fn put(grid: &mut Vec<Vec<Cell>>, x: isize, y: isize, ch: char, color: &'static str,
       depth: f32, width: usize, height: usize) {
    if x < 0 || x >= width as isize || y < 0 || y >= height as isize { return; }
    let (x, y) = (x as usize, y as usize);
    if depth >= grid[y][x].depth {
        grid[y][x] = Cell { ch, color, depth };
    }
}

fn clip(s: &str, max: usize) -> String {
    let v: Vec<char> = s.chars().collect();
    if v.len() <= max { s.to_string() }
    else { v[..max-1].iter().collect::<String>() + "…" }
}
