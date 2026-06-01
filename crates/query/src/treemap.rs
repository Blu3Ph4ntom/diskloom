use diskloom_core::EntryId;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TreemapBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreemapItem {
    pub id: EntryId,
    pub label: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TreemapRect {
    pub id: EntryId,
    pub label: String,
    pub size: u64,
    pub bounds: TreemapBounds,
}

#[must_use]
pub fn layout_treemap(items: &[TreemapItem], bounds: TreemapBounds) -> Vec<TreemapRect> {
    let total = items
        .iter()
        .map(|item| item.size)
        .filter(|size| *size > 0)
        .sum::<u64>();
    if total == 0 || bounds.width <= 0.0 || bounds.height <= 0.0 {
        return Vec::new();
    }

    let mut rects = Vec::with_capacity(items.len());
    slice_layout(items, total as f32, bounds, &mut rects);
    rects
}

fn slice_layout(
    items: &[TreemapItem],
    total: f32,
    bounds: TreemapBounds,
    rects: &mut Vec<TreemapRect>,
) {
    let mut cursor = if bounds.width >= bounds.height {
        bounds.x
    } else {
        bounds.y
    };

    for item in items.iter().filter(|item| item.size > 0) {
        let ratio = item.size as f32 / total;
        let item_bounds = if bounds.width >= bounds.height {
            let width = bounds.width * ratio;
            let item_bounds = TreemapBounds {
                x: cursor,
                y: bounds.y,
                width,
                height: bounds.height,
            };
            cursor += width;
            item_bounds
        } else {
            let height = bounds.height * ratio;
            let item_bounds = TreemapBounds {
                x: bounds.x,
                y: cursor,
                width: bounds.width,
                height,
            };
            cursor += height;
            item_bounds
        };

        rects.push(TreemapRect {
            id: item.id,
            label: item.label.clone(),
            size: item.size,
            bounds: item_bounds,
        });
    }
}

#[cfg(test)]
mod tests {
    use diskloom_core::EntryId;

    use super::{TreemapBounds, TreemapItem, layout_treemap};

    #[test]
    fn layout_treemap_should_allocate_width_proportionally() {
        let rects = layout_treemap(
            &[
                TreemapItem {
                    id: EntryId(1),
                    label: "a".to_owned(),
                    size: 75,
                },
                TreemapItem {
                    id: EntryId(2),
                    label: "b".to_owned(),
                    size: 25,
                },
            ],
            TreemapBounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 50.0,
            },
        );

        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].bounds.width, 75.0);
        assert_eq!(rects[1].bounds.x, 75.0);
    }

    #[test]
    fn layout_treemap_should_skip_zero_sized_items() {
        let rects = layout_treemap(
            &[TreemapItem {
                id: EntryId(1),
                label: "empty".to_owned(),
                size: 0,
            }],
            TreemapBounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 50.0,
            },
        );

        assert!(rects.is_empty());
    }
}
