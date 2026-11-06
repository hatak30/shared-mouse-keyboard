//! Screen layout: every machine occupies one cell of an integer grid.
//! Machines in horizontally / vertically adjacent cells are neighbours and
//! the cursor can travel between them over the shared edge.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn opposite(self) -> Self {
        match self {
            Direction::Left => Direction::Right,
            Direction::Right => Direction::Left,
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
        }
    }
    fn offset(self) -> (i32, i32) {
        match self {
            Direction::Left => (-1, 0),
            Direction::Right => (1, 0),
            Direction::Up => (0, -1),
            Direction::Down => (0, 1),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub node_id: Uuid,
    pub name: String,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    /// Monotonic version (ms since epoch of last edit). Newest layout wins
    /// when peers synchronise.
    pub version: u64,
    pub placements: Vec<Placement>,
}

impl Layout {
    pub fn position(&self, node: Uuid) -> Option<(i32, i32)> {
        self.placements
            .iter()
            .find(|p| p.node_id == node)
            .map(|p| (p.x, p.y))
    }

    pub fn neighbor(&self, node: Uuid, dir: Direction) -> Option<Uuid> {
        let (x, y) = self.position(node)?;
        let (dx, dy) = dir.offset();
        self.placements
            .iter()
            .find(|p| p.x == x + dx && p.y == y + dy)
            .map(|p| p.node_id)
    }

    /// Make sure `node` is part of the layout, placing it in the first free
    /// cell to the right of existing machines.
    pub fn ensure(&mut self, node: Uuid, name: &str) -> bool {
        if let Some(p) = self.placements.iter_mut().find(|p| p.node_id == node) {
            if p.name != name {
                p.name = name.to_string();
            }
            return false;
        }
        let x = self.placements.iter().map(|p| p.x + 1).max().unwrap_or(0);
        self.placements.push(Placement {
            node_id: node,
            name: name.to_string(),
            x,
            y: 0,
        });
        true
    }

    /// Validate an edited layout: no two machines may share a cell.
    pub fn validate(&self) -> Result<(), String> {
        for (i, a) in self.placements.iter().enumerate() {
            for b in &self.placements[i + 1..] {
                if a.node_id == b.node_id {
                    return Err(format!("{} appears twice", a.name));
                }
                if a.x == b.x && a.y == b.y {
                    return Err(format!("{} and {} occupy the same cell", a.name, b.name));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighbours() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let layout = Layout {
            version: 1,
            placements: vec![
                Placement { node_id: a, name: "a".into(), x: 0, y: 0 },
                Placement { node_id: b, name: "b".into(), x: 1, y: 0 },
                Placement { node_id: c, name: "c".into(), x: 0, y: 1 },
            ],
        };
        assert_eq!(layout.neighbor(a, Direction::Right), Some(b));
        assert_eq!(layout.neighbor(b, Direction::Left), Some(a));
        assert_eq!(layout.neighbor(a, Direction::Down), Some(c));
        assert_eq!(layout.neighbor(a, Direction::Up), None);
        assert_eq!(layout.neighbor(b, Direction::Down), None);
        assert!(layout.validate().is_ok());
    }

    #[test]
    fn overlapping_is_invalid() {
        let layout = Layout {
            version: 1,
            placements: vec![
                Placement { node_id: Uuid::new_v4(), name: "a".into(), x: 0, y: 0 },
                Placement { node_id: Uuid::new_v4(), name: "b".into(), x: 0, y: 0 },
            ],
        };
        assert!(layout.validate().is_err());
    }
}
