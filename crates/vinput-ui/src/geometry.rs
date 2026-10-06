//! Points, sizes and rectangles, in device-independent units: one unit is one
//! pixel on a display at 100 %.

/// A point.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    /// Distance from the left edge.
    pub x: f32,
    /// Distance from the top edge.
    pub y: f32,
}

impl Point {
    /// The point at `x`, `y`.
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A width and a height.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    /// The width.
    pub w: f32,
    /// The height.
    pub h: f32,
}

impl Size {
    /// A size of `w` by `h`.
    #[must_use]
    pub const fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }
}

/// A rectangle with its edges along the axes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// Its left edge.
    pub x: f32,
    /// Its top edge.
    pub y: f32,
    /// Its width.
    pub w: f32,
    /// Its height.
    pub h: f32,
}

impl Rect {
    /// The rectangle with its top left corner at `x`, `y`.
    #[must_use]
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Its right edge.
    #[must_use]
    pub fn right(self) -> f32 {
        self.x + self.w
    }

    /// Its bottom edge.
    #[must_use]
    pub fn bottom(self) -> f32 {
        self.y + self.h
    }

    /// Its middle.
    #[must_use]
    pub fn center(self) -> Point {
        Point::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// Whether `point` is inside it.
    #[must_use]
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }

    /// The rectangle moved by `dx`, `dy`.
    #[must_use]
    pub fn moved(self, dx: f32, dy: f32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// The rectangle with every edge moved inwards by `by`; outwards if it
    /// is negative.
    #[must_use]
    pub fn inset(self, by: f32) -> Self {
        self.inset_xy(by, by)
    }

    /// The rectangle with its sides moved inwards by `dx` and its top and
    /// bottom by `dy`.
    #[must_use]
    pub fn inset_xy(self, dx: f32, dy: f32) -> Self {
        Self::new(
            self.x + dx,
            self.y + dy,
            (self.w - 2.0 * dx).max(0.0),
            (self.h - 2.0 * dy).max(0.0),
        )
    }

    /// A rectangle of `size` with the same middle, as far as the height
    /// goes, and the same left edge.
    #[must_use]
    pub fn left_center(self, size: Size) -> Self {
        Self::new(self.x, self.y + (self.h - size.h) / 2.0, size.w, size.h)
    }

    /// A rectangle of `size` with the same middle.
    #[must_use]
    pub fn centered(self, size: Size) -> Self {
        Self::new(
            self.x + (self.w - size.w) / 2.0,
            self.y + (self.h - size.h) / 2.0,
            size.w,
            size.h,
        )
    }

    /// Whether any of it lies inside `other`.
    #[must_use]
    pub fn touches(self, other: Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}
