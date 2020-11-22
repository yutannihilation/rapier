use crate::geometry::{ColliderHandle, ColliderSet, Ray, AABB};
use crate::geometry::{WRay, WAABB};
use crate::math::Point;
#[cfg(feature = "dim3")]
use crate::math::Vector;
use crate::simd::{SimdFloat, SIMD_WIDTH};
use ncollide::bounding_volume::BoundingVolume;
use simba::simd::{SimdBool, SimdValue};
use std::collections::VecDeque;
use std::ops::Range;

pub trait IndexedData: Copy {
    fn default() -> Self;
    fn index(&self) -> usize;
}

impl IndexedData for usize {
    fn default() -> Self {
        u32::MAX as usize
    }

    fn index(&self) -> usize {
        *self
    }
}

impl IndexedData for ColliderHandle {
    fn default() -> Self {
        ColliderSet::invalid_handle()
    }

    fn index(&self) -> usize {
        self.into_raw_parts().0
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
struct NodeIndex {
    index: u32, // Index of the addressed node in the `nodes` array.
    lane: u8,   // SIMD lane of the addressed node.
}

impl NodeIndex {
    fn new(index: u32, lane: u8) -> Self {
        Self { index, lane }
    }

    fn invalid() -> Self {
        Self {
            index: u32::MAX,
            lane: 0,
        }
    }
}

#[derive(Copy, Clone, Debug)]
#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
pub struct WQuadtreeNode {
    waabb: WAABB,
    // Index of the nodes of the 4 nodes represented by self.
    // If this is a leaf, it contains the proxy ids instead.
    children: [u32; 4],
    parent: NodeIndex,
    leaf: bool,  // TODO: pack this with the NodexIndex.lane?
    dirty: bool, // TODO: move this to a separate bitvec?
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
pub struct WQuadtreeProxy<T> {
    node: NodeIndex,
    data: T, // The collider data. TODO: only set the collider generation here?
}

impl<T: IndexedData> WQuadtreeProxy<T> {
    fn invalid() -> Self {
        Self {
            node: NodeIndex::invalid(),
            data: T::default(),
        }
    }
}

#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
#[derive(Clone, Debug)]
pub struct WQuadtree<T> {
    pub nodes: Vec<WQuadtreeNode>,
    pub dirty_nodes: VecDeque<u32>,
    pub proxies: Vec<WQuadtreeProxy<T>>,
}

// FIXME: this should be generic too.
impl WQuadtree<ColliderHandle> {
    pub fn pre_update(&mut self, data: ColliderHandle) {
        let id = data.into_raw_parts().0;
        let node_id = self.proxies[id].node.index;
        let node = &mut self.nodes[node_id as usize];
        if !node.dirty {
            node.dirty = true;
            self.dirty_nodes.push_back(node_id);
        }
    }

    pub fn update(&mut self, colliders: &ColliderSet, dilation_factor: f32) {
        // Loop on the dirty leaves.
        let dilation_factor = SimdFloat::splat(dilation_factor);

        while let Some(id) = self.dirty_nodes.pop_front() {
            // NOTE: this will data the case where we reach the root of the tree.
            if let Some(node) = self.nodes.get(id as usize) {
                // Compute the new WAABB.
                let mut new_aabbs = [AABB::new_invalid(); SIMD_WIDTH];
                for (child_id, new_aabb) in node.children.iter().zip(new_aabbs.iter_mut()) {
                    if node.leaf {
                        // We are in a leaf: compute the colliders' AABBs.
                        if let Some(proxy) = self.proxies.get(*child_id as usize) {
                            let collider = &colliders[proxy.data];
                            *new_aabb = collider.compute_aabb();
                        }
                    } else {
                        // We are in an internal node: compute the children's AABBs.
                        if let Some(node) = self.nodes.get(*child_id as usize) {
                            *new_aabb = node.waabb.to_merged_aabb();
                        }
                    }
                }

                let node = &mut self.nodes[id as usize];
                let new_waabb = WAABB::from(new_aabbs);
                if !node.waabb.contains(&new_waabb).all() {
                    node.waabb = new_waabb;
                    node.waabb.dilate_by_factor(dilation_factor);
                    self.dirty_nodes.push_back(node.parent.index);
                }
                node.dirty = false;
            }
        }
    }
}

impl<T: IndexedData> WQuadtree<T> {
    pub fn new() -> Self {
        WQuadtree {
            nodes: Vec::new(),
            dirty_nodes: VecDeque::new(),
            proxies: Vec::new(),
        }
    }

    pub fn clear_and_rebuild(
        &mut self,
        data: impl ExactSizeIterator<Item = (T, AABB)>,
        dilation_factor: f32,
    ) {
        self.nodes.clear();
        self.proxies.clear();

        // Create proxies.
        let mut indices = Vec::with_capacity(data.len());
        let mut aabbs = vec![AABB::new_invalid(); data.len()];
        self.proxies = vec![WQuadtreeProxy::invalid(); data.len()];

        for (data, aabb) in data {
            let index = data.index();
            if index >= self.proxies.len() {
                self.proxies.resize(index + 1, WQuadtreeProxy::invalid());
                aabbs.resize(index + 1, AABB::new_invalid());
            }

            self.proxies[index].data = data;
            aabbs[index] = aabb;
            indices.push(index);
        }

        // Build the tree recursively.
        let root_node = WQuadtreeNode {
            waabb: WAABB::new_invalid(),
            children: [1, u32::MAX, u32::MAX, u32::MAX],
            parent: NodeIndex::invalid(),
            leaf: false,
            dirty: false,
        };

        self.nodes.push(root_node);
        let root_id = NodeIndex::new(0, 0);
        let (_, aabb) = self.do_recurse_build(&mut indices, &aabbs, root_id, dilation_factor);
        self.nodes[0].waabb = WAABB::from([
            aabb,
            AABB::new_invalid(),
            AABB::new_invalid(),
            AABB::new_invalid(),
        ]);
    }

    fn do_recurse_build(
        &mut self,
        indices: &mut [usize],
        aabbs: &[AABB],
        parent: NodeIndex,
        dilation_factor: f32,
    ) -> (u32, AABB) {
        if indices.len() <= 4 {
            // Leaf case.
            let my_id = self.nodes.len();
            let mut my_aabb = AABB::new_invalid();
            let mut leaf_aabbs = [AABB::new_invalid(); 4];
            let mut proxy_ids = [u32::MAX; 4];

            for (k, id) in indices.iter().enumerate() {
                my_aabb.merge(&aabbs[*id]);
                leaf_aabbs[k] = aabbs[*id];
                proxy_ids[k] = *id as u32;
                self.proxies[*id].node = NodeIndex::new(my_id as u32, k as u8);
            }

            let mut node = WQuadtreeNode {
                waabb: WAABB::from(leaf_aabbs),
                children: proxy_ids,
                parent,
                leaf: true,
                dirty: false,
            };

            node.waabb
                .dilate_by_factor(SimdFloat::splat(dilation_factor));
            self.nodes.push(node);
            return (my_id as u32, my_aabb);
        }

        // Compute the center and variance along each dimension.
        // In 3D we compute the variance to not-subdivide the dimension with lowest variance.
        // Therefore variance computation is not needed in 2D because we only have 2 dimension
        // to split in the first place.
        let mut center = Point::origin();
        #[cfg(feature = "dim3")]
        let mut variance = Vector::zeros();

        let denom = 1.0 / (indices.len() as f32);

        for i in &*indices {
            let coords = aabbs[*i].center().coords;
            center += coords * denom;
            #[cfg(feature = "dim3")]
            {
                variance += coords.component_mul(&coords) * denom;
            }
        }

        #[cfg(feature = "dim3")]
        {
            variance = variance - center.coords.component_mul(&center.coords);
        }

        // Find the axis with minimum variance. This is the axis along
        // which we are **not** subdividing our set.
        #[allow(unused_mut)] // Does not need to be mutable in 2D.
        let mut subdiv_dims = [0, 1];
        #[cfg(feature = "dim3")]
        {
            let min = variance.imin();
            subdiv_dims[0] = (min + 1) % 3;
            subdiv_dims[1] = (min + 2) % 3;
        }

        // Split the set along the two subdiv_dims dimensions.
        // TODO: should we split wrt. the median instead of the average?
        // TODO: we should ensure each subslice contains at least 4 elements each (or less if
        // indices has less than 16 elements in the first place.
        let (left, right) = split_indices_wrt_dim(indices, &aabbs, &center, subdiv_dims[0]);

        let (left_bottom, left_top) = split_indices_wrt_dim(left, &aabbs, &center, subdiv_dims[1]);
        let (right_bottom, right_top) =
            split_indices_wrt_dim(right, &aabbs, &center, subdiv_dims[1]);

        // println!(
        //     "Recursing on children: {}, {}, {}, {}",
        //     left_bottom.len(),
        //     left_top.len(),
        //     right_bottom.len(),
        //     right_top.len()
        // );

        let node = WQuadtreeNode {
            waabb: WAABB::new_invalid(),
            children: [0; 4], // Will be set after the recursive call
            parent,
            leaf: false,
            dirty: false,
        };

        let id = self.nodes.len() as u32;
        self.nodes.push(node);

        // Recurse!
        let a = self.do_recurse_build(left_bottom, aabbs, NodeIndex::new(id, 0), dilation_factor);
        let b = self.do_recurse_build(left_top, aabbs, NodeIndex::new(id, 1), dilation_factor);
        let c = self.do_recurse_build(right_bottom, aabbs, NodeIndex::new(id, 2), dilation_factor);
        let d = self.do_recurse_build(right_top, aabbs, NodeIndex::new(id, 3), dilation_factor);

        // Now we know the indices of the grand-nodes.
        self.nodes[id as usize].children = [a.0, b.0, c.0, d.0];
        self.nodes[id as usize].waabb = WAABB::from([a.1, b.1, c.1, d.1]);
        self.nodes[id as usize]
            .waabb
            .dilate_by_factor(SimdFloat::splat(dilation_factor));

        // TODO: will this chain of .merged be properly optimized?
        let my_aabb = a.1.merged(&b.1).merged(&c.1).merged(&d.1);
        (id, my_aabb)
    }

    // FIXME: implement a visitor pattern to merge intersect_aabb
    // and intersect_ray into a single method.
    pub fn intersect_aabb(&self, aabb: &AABB, out: &mut Vec<T>) {
        if self.nodes.is_empty() {
            return;
        }

        // Special case for the root.
        let mut stack = vec![0u32];
        let waabb = WAABB::splat(*aabb);
        while let Some(inode) = stack.pop() {
            let node = self.nodes[inode as usize];
            let intersections = node.waabb.intersects(&waabb);
            let bitmask = intersections.bitmask();

            for ii in 0..SIMD_WIDTH {
                if (bitmask & (1 << ii)) != 0 {
                    if node.leaf {
                        // We found a leaf!
                        // Unfortunately, invalid AABBs return a intersection as well.
                        if let Some(proxy) = self.proxies.get(node.children[ii] as usize) {
                            out.push(proxy.data);
                        }
                    } else {
                        // Internal node, visit the child.
                        // Unfortunately, we have this check because invalid AABBs
                        // return a intersection as well.
                        if node.children[ii] as usize <= self.nodes.len() {
                            stack.push(node.children[ii]);
                        }
                    }
                }
            }
        }
    }

    pub fn cast_ray(&self, ray: &Ray, max_toi: f32, out: &mut Vec<T>) {
        if self.nodes.is_empty() {
            return;
        }

        // Special case for the root.
        let mut stack = vec![0u32];
        let wray = WRay::splat(*ray);
        let wmax_toi = SimdFloat::splat(max_toi);
        while let Some(inode) = stack.pop() {
            let node = self.nodes[inode as usize];
            let hits = node.waabb.intersects_ray(&wray, wmax_toi);
            let bitmask = hits.bitmask();

            for ii in 0..SIMD_WIDTH {
                if (bitmask & (1 << ii)) != 0 {
                    if node.leaf {
                        // We found a leaf!
                        // Unfortunately, invalid AABBs return a hit as well.
                        if let Some(proxy) = self.proxies.get(node.children[ii] as usize) {
                            out.push(proxy.data);
                        }
                    } else {
                        // Internal node, visit the child.
                        // Un fortunately, we have this check because invalid AABBs
                        // return a hit as well.
                        if node.children[ii] as usize <= self.nodes.len() {
                            stack.push(node.children[ii]);
                        }
                    }
                }
            }
        }
    }
}

#[allow(dead_code)]
struct WQuadtreeIncrementalBuilderStep {
    range: Range<usize>,
    parent: NodeIndex,
}

#[allow(dead_code)]
struct WQuadtreeIncrementalBuilder<T> {
    quadtree: WQuadtree<T>,
    to_insert: Vec<WQuadtreeIncrementalBuilderStep>,
    aabbs: Vec<AABB>,
    indices: Vec<usize>,
}

#[allow(dead_code)]
impl<T: IndexedData> WQuadtreeIncrementalBuilder<T> {
    pub fn new() -> Self {
        Self {
            quadtree: WQuadtree::new(),
            to_insert: Vec::new(),
            aabbs: Vec::new(),
            indices: Vec::new(),
        }
    }

    pub fn update_single_depth(&mut self) {
        if let Some(to_insert) = self.to_insert.pop() {
            let indices = &mut self.indices[to_insert.range];

            // Leaf case.
            if indices.len() <= 4 {
                let id = self.quadtree.nodes.len();
                let mut aabb = AABB::new_invalid();
                let mut leaf_aabbs = [AABB::new_invalid(); 4];
                let mut proxy_ids = [u32::MAX; 4];

                for (k, id) in indices.iter().enumerate() {
                    aabb.merge(&self.aabbs[*id]);
                    leaf_aabbs[k] = self.aabbs[*id];
                    proxy_ids[k] = *id as u32;
                }

                let node = WQuadtreeNode {
                    waabb: WAABB::from(leaf_aabbs),
                    children: proxy_ids,
                    parent: to_insert.parent,
                    leaf: true,
                    dirty: false,
                };

                self.quadtree.nodes[to_insert.parent.index as usize].children
                    [to_insert.parent.lane as usize] = id as u32;
                self.quadtree.nodes[to_insert.parent.index as usize]
                    .waabb
                    .replace(to_insert.parent.lane as usize, aabb);
                self.quadtree.nodes.push(node);
                return;
            }

            // Compute the center and variance along each dimension.
            // In 3D we compute the variance to not-subdivide the dimension with lowest variance.
            // Therefore variance computation is not needed in 2D because we only have 2 dimension
            // to split in the first place.
            let mut center = Point::origin();
            #[cfg(feature = "dim3")]
            let mut variance = Vector::zeros();

            let denom = 1.0 / (indices.len() as f32);
            let mut aabb = AABB::new_invalid();

            for i in &*indices {
                let coords = self.aabbs[*i].center().coords;
                aabb.merge(&self.aabbs[*i]);
                center += coords * denom;
                #[cfg(feature = "dim3")]
                {
                    variance += coords.component_mul(&coords) * denom;
                }
            }

            #[cfg(feature = "dim3")]
            {
                variance = variance - center.coords.component_mul(&center.coords);
            }

            // Find the axis with minimum variance. This is the axis along
            // which we are **not** subdividing our set.
            #[allow(unused_mut)] // Does not need to be mutable in 2D.
            let mut subdiv_dims = [0, 1];
            #[cfg(feature = "dim3")]
            {
                let min = variance.imin();
                subdiv_dims[0] = (min + 1) % 3;
                subdiv_dims[1] = (min + 2) % 3;
            }

            // Split the set along the two subdiv_dims dimensions.
            // TODO: should we split wrt. the median instead of the average?
            // TODO: we should ensure each subslice contains at least 4 elements each (or less if
            // indices has less than 16 elements in the first place.
            let (left, right) =
                split_indices_wrt_dim(indices, &self.aabbs, &center, subdiv_dims[0]);

            let (left_bottom, left_top) =
                split_indices_wrt_dim(left, &self.aabbs, &center, subdiv_dims[1]);
            let (right_bottom, right_top) =
                split_indices_wrt_dim(right, &self.aabbs, &center, subdiv_dims[1]);

            let node = WQuadtreeNode {
                waabb: WAABB::new_invalid(),
                children: [0; 4], // Will be set after the recursive call
                parent: to_insert.parent,
                leaf: false,
                dirty: false,
            };

            let id = self.quadtree.nodes.len() as u32;
            self.quadtree.nodes.push(node);

            // Recurse!
            let a = left_bottom.len();
            let b = a + left_top.len();
            let c = b + right_bottom.len();
            let d = c + right_top.len();
            self.to_insert.push(WQuadtreeIncrementalBuilderStep {
                range: 0..a,
                parent: NodeIndex::new(id, 0),
            });
            self.to_insert.push(WQuadtreeIncrementalBuilderStep {
                range: a..b,
                parent: NodeIndex::new(id, 1),
            });
            self.to_insert.push(WQuadtreeIncrementalBuilderStep {
                range: b..c,
                parent: NodeIndex::new(id, 2),
            });
            self.to_insert.push(WQuadtreeIncrementalBuilderStep {
                range: c..d,
                parent: NodeIndex::new(id, 3),
            });

            self.quadtree.nodes[to_insert.parent.index as usize].children
                [to_insert.parent.lane as usize] = id as u32;
            self.quadtree.nodes[to_insert.parent.index as usize]
                .waabb
                .replace(to_insert.parent.lane as usize, aabb);
        }
    }
}

fn split_indices_wrt_dim<'a>(
    indices: &'a mut [usize],
    aabbs: &[AABB],
    split_point: &Point<f32>,
    dim: usize,
) -> (&'a mut [usize], &'a mut [usize]) {
    let mut icurr = 0;
    let mut ilast = indices.len();

    // The loop condition we can just do 0..indices.len()
    // instead of the test icurr < ilast because we know
    // we will iterate exactly once per index.
    for _ in 0..indices.len() {
        let i = indices[icurr];
        let center = aabbs[i].center();

        if center[dim] > split_point[dim] {
            ilast -= 1;
            indices.swap(icurr, ilast);
        } else {
            icurr += 1;
        }
    }

    if icurr == 0 || icurr == indices.len() {
        // We don't want to return one empty set. But
        // this can happen if all the coordinates along the
        // given dimension are equal.
        // In this is the case, we just split in the middle.
        let half = indices.len() / 2;
        indices.split_at_mut(half)
    } else {
        indices.split_at_mut(icurr)
    }
}

#[cfg(test)]
mod test {
    use crate::geometry::{WQuadtree, AABB};
    use crate::math::{Point, Vector};

    #[test]
    fn multiple_identical_AABB_stack_overflow() {
        // A stack overflow was caused during the construction of the
        // WAABB tree with more than four AABB with the same center.
        let aabb = AABB::new(Point::origin(), Vector::repeat(1.0).into());

        for k in 0..20 {
            let mut tree = WQuadtree::new();
            tree.clear_and_rebuild((0..k).map(|i| (i, aabb)), 0.0);
        }
    }
}
