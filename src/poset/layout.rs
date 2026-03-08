use super::Poset;
use std::collections::HashMap;
use std::f32::consts::TAU;

pub fn assign_positions(poset: &mut Poset) {
    if poset.nodes.is_empty() { return; }

    let layers = compute_layers(poset);
    let max_layer = *layers.values().max().unwrap_or(&0);

    let mut by_layer: HashMap<usize, Vec<usize>> = HashMap::new();
    for (&id, &layer) in &layers {
        by_layer.entry(layer).or_default().push(id);
    }

    for (&layer, ids) in &by_layer {
        let z = if max_layer == 0 { 0.0 }
                else { (layer as f32 / max_layer as f32) * 4.0 - 2.0 };

        let count = ids.len();
        let mut sorted = ids.clone();
        sorted.sort();

        for (i, &id) in sorted.iter().enumerate() {
            let (x, y) = if count == 1 {
                (0.0f32, 0.0f32)
            } else {
                let angle = (i as f32 / count as f32) * TAU;
                let r = 1.5 + (count as f32 * 0.3).min(2.0);
                (angle.cos() * r, angle.sin() * r * 0.5)
            };
            if let Some(n) = poset.nodes.iter_mut().find(|n| n.id == id) {
                n.pos = [x, y, z];
            }
        }
    }
}

pub fn compute_layers(poset: &Poset) -> HashMap<usize, usize> {
    use std::collections::{HashSet, VecDeque};
    let mut layers: HashMap<usize, usize> = HashMap::new();
    let has_pred: HashSet<usize> = poset.edges.iter().map(|(_, b)| *b).collect();
    let roots: Vec<usize> = poset.nodes.iter()
        .filter(|n| !has_pred.contains(&n.id)).map(|n| n.id).collect();

    let mut queue = VecDeque::new();
    for &r in &roots { layers.insert(r, 0); queue.push_back(r); }

    while let Some(id) = queue.pop_front() {
        let cur = *layers.get(&id).unwrap_or(&0);
        for &(before, after) in &poset.edges {
            if before == id {
                let e = layers.entry(after).or_insert(0);
                if cur + 1 > *e { *e = cur + 1; queue.push_back(after); }
            }
        }
    }
    for n in &poset.nodes { layers.entry(n.id).or_insert(0); }
    layers
}
