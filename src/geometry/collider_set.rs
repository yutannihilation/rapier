use crate::data::arena::Arena;
use crate::data::pubsub::PubSub;
use crate::dynamics::{RigidBodyHandle, RigidBodySet};
use crate::geometry::{Collider, ColliderGraphIndex};
use std::ops::{Index, IndexMut};

/// The unique identifier of a collider added to a collider set.
pub type ColliderHandle = crate::data::arena::Index;

#[derive(Copy, Clone, Debug)]
#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
pub(crate) struct RemovedCollider {
    pub handle: ColliderHandle,
    pub(crate) proxy_index: usize,
}

#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
#[derive(Clone)]
/// A set of colliders that can be handled by a physics `World`.
pub struct ColliderSet {
    pub(crate) removed_colliders: PubSub<RemovedCollider>,
    pub(crate) colliders: Arena<Collider>,
}

impl ColliderSet {
    /// Create a new empty set of colliders.
    pub fn new() -> Self {
        ColliderSet {
            removed_colliders: PubSub::new(),
            colliders: Arena::new(),
        }
    }

    /// An always-invalid collider handle.
    pub fn invalid_handle() -> ColliderHandle {
        ColliderHandle::from_raw_parts(crate::INVALID_USIZE, crate::INVALID_U64)
    }

    /// Iterate through all the colliders on this set.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (ColliderHandle, &Collider)> {
        self.colliders.iter()
    }

    /// The number of colliders on this set.
    pub fn len(&self) -> usize {
        self.colliders.len()
    }

    /// Is this collider handle valid?
    pub fn contains(&self, handle: ColliderHandle) -> bool {
        self.colliders.contains(handle)
    }

    /// Inserts a new collider to this set and retrieve its handle.
    pub fn insert(
        &mut self,
        mut coll: Collider,
        parent_handle: RigidBodyHandle,
        bodies: &mut RigidBodySet,
    ) -> ColliderHandle {
        // Make sure the internal links are reset, they may not be
        // if this rigid-body was obtained by cloning another one.
        coll.reset_internal_references();

        coll.parent = parent_handle;
        let parent = bodies
            .get_mut_internal(parent_handle)
            .expect("Parent rigid body not found.");
        coll.position = parent.position * coll.delta;
        coll.predicted_position = parent.predicted_position * coll.delta;
        let handle = self.colliders.insert(coll);
        let coll = self.colliders.get(handle).unwrap();
        parent.add_collider_internal(handle, &coll);
        bodies.activate(parent_handle);
        handle
    }

    /// Remove a collider from this set and update its parent accordingly.
    pub fn remove(
        &mut self,
        handle: ColliderHandle,
        bodies: &mut RigidBodySet,
    ) -> Option<Collider> {
        let collider = self.colliders.remove(handle)?;

        /*
         * Delete the collider from its parent body.
         */
        if let Some(parent) = bodies.get_mut_internal(collider.parent) {
            parent.remove_collider_internal(handle, &collider);
            bodies.wake_up(collider.parent, true);
        }

        /*
         * Publish removal.
         */
        let message = RemovedCollider {
            handle,
            proxy_index: collider.proxy_index,
        };

        self.removed_colliders.publish(message);

        Some(collider)
    }

    /// Gets the collider with the given handle without a known generation.
    ///
    /// This is useful when you know you want the collider at position `i` but
    /// don't know what is its current generation number. Generation numbers are
    /// used to protect from the ABA problem because the collider position `i`
    /// are recycled between two insertion and a removal.
    ///
    /// Using this is discouraged in favor of `self.get(handle)` which does not
    /// suffer form the ABA problem.
    pub fn get_unknown_gen(&self, i: usize) -> Option<(&Collider, ColliderHandle)> {
        self.colliders.get_unknown_gen(i)
    }

    /// Gets a mutable reference to the collider with the given handle without a known generation.
    ///
    /// This is useful when you know you want the collider at position `i` but
    /// don't know what is its current generation number. Generation numbers are
    /// used to protect from the ABA problem because the collider position `i`
    /// are recycled between two insertion and a removal.
    ///
    /// Using this is discouraged in favor of `self.get_mut(handle)` which does not
    /// suffer form the ABA problem.
    pub fn get_unknown_gen_mut(&mut self, i: usize) -> Option<(&mut Collider, ColliderHandle)> {
        self.colliders.get_unknown_gen_mut(i)
    }

    /// Get the collider with the given handle.
    pub fn get(&self, handle: ColliderHandle) -> Option<&Collider> {
        self.colliders.get(handle)
    }

    /// Gets a mutable reference to the collider with the given handle.
    pub fn get_mut(&mut self, handle: ColliderHandle) -> Option<&mut Collider> {
        self.colliders.get_mut(handle)
    }

    pub(crate) fn get2_mut_internal(
        &mut self,
        h1: ColliderHandle,
        h2: ColliderHandle,
    ) -> (Option<&mut Collider>, Option<&mut Collider>) {
        self.colliders.get2_mut(h1, h2)
    }

    // pub fn iter_mut(&mut self) -> impl Iterator<Item = (ColliderHandle, ColliderMut)> {
    //     //        let sender = &self.activation_channel_sender;
    //     self.colliders.iter_mut().map(move |(h, rb)| {
    //         (h, ColliderMut::new(h, rb /*sender.clone()*/))
    //     })
    // }

    //    pub(crate) fn iter_mut_internal(
    //        &mut self,
    //    ) -> impl Iterator<Item = (ColliderHandle, &mut Collider)> {
    //        self.colliders.iter_mut()
    //    }
}

impl Index<ColliderHandle> for ColliderSet {
    type Output = Collider;

    fn index(&self, index: ColliderHandle) -> &Collider {
        &self.colliders[index]
    }
}

impl IndexMut<ColliderHandle> for ColliderSet {
    fn index_mut(&mut self, index: ColliderHandle) -> &mut Collider {
        &mut self.colliders[index]
    }
}
