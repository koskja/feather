use std::{path::PathBuf, sync::Arc};

use ahash::{AHashMap, AHashSet};
use base::{
    categories::SupportType, BlockPosition, Chunk, ChunkHandle, ChunkLock, ChunkPosition,
    FacingCardinal, FacingCardinalAndDown, FacingCubic, CHUNK_HEIGHT,
};
use blocks::{BlockId, BlockKind};
use ecs::{Ecs, SysResult};
use libcraft_core::BlockFace;
use parking_lot::{RwLockReadGuard, RwLockWriteGuard};
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;
use base::anvil::player::PlayerData;

use worldgen::{ComposableGenerator, WorldGenerator};

use crate::{
    chunk::cache::ChunkCache,
    chunk::worker::{ChunkWorker, LoadRequest, SaveRequest},
    events::ChunkLoadEvent,
};

/// Stores all blocks and chunks in a world,
/// along with global world data like weather, time,
/// and the [`WorldSource`](crate::world_source::WorldSource).
///
/// NB: _not_ what most Rust ECSs call "world."
/// This does not store entities; it only contains blocks.
pub struct World {
    chunk_map: ChunkMap,
    pub cache: ChunkCache,
    chunk_worker: ChunkWorker,
    loading_chunks: AHashSet<ChunkPosition>,
    canceled_chunk_loads: AHashSet<ChunkPosition>,
    world_dir: PathBuf,
}

impl Default for World {
    fn default() -> Self {
        Self {
            chunk_map: ChunkMap::new(),
            chunk_worker: ChunkWorker::new(
                "world",
                Arc::new(ComposableGenerator::default_with_seed(0)),
            ),
            cache: ChunkCache::new(),
            loading_chunks: AHashSet::new(),
            canceled_chunk_loads: AHashSet::new(),
            world_dir: "world".into(),
        }
    }
}

impl World {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_gen_and_path(
        generator: Arc<dyn WorldGenerator>,
        world_dir: impl Into<PathBuf> + Clone,
    ) -> Self {
        Self {
            world_dir: world_dir.clone().into(),
            chunk_worker: ChunkWorker::new(world_dir, generator),
            ..Default::default()
        }
    }

    /// Queues the given chunk to be loaded. If the chunk was cached, it is loaded immediately.
    pub fn queue_chunk_load(&mut self, req: LoadRequest) {
        let pos = req.pos;
        if self.cache.contains(&pos) {
            // Move the chunk from the cache to the map
            self.chunk_map
                .0
                .insert(pos, self.cache.remove(pos).unwrap());
            self.chunk_map.chunk_handle_at(pos).unwrap().set_loaded();
        } else {
            self.loading_chunks.insert(req.pos);
            self.chunk_worker.queue_load(req);
        }
    }

    /// Loads any chunks that have been loaded asynchronously
    /// after a call to [`World::queue_chunk_load`].
    pub fn load_chunks(&mut self, ecs: &mut Ecs) -> SysResult {
        while let Some(loaded) = self.chunk_worker.poll_loaded_chunk()? {
            self.loading_chunks.remove(&loaded.pos);
            if self.canceled_chunk_loads.remove(&loaded.pos) {
                continue;
            }
            let chunk = loaded.chunk;

            self.chunk_map.insert_chunk(chunk);
            ecs.insert_event(ChunkLoadEvent {
                chunk: Arc::clone(&self.chunk_map.0[&loaded.pos]),
                position: loaded.pos,
            });
            log::trace!("Loaded chunk {:?}", loaded.pos);
        }
        Ok(())
    }

    /// Unloads the given chunk.
    pub fn unload_chunk(&mut self, pos: ChunkPosition) -> anyhow::Result<()> {
        if let Some((pos, handle)) = self.chunk_map.0.remove_entry(&pos) {
            handle.set_unloaded()?;
            self.chunk_worker.queue_chunk_save(SaveRequest {
                pos,
                chunk: handle.clone(),
                entities: vec![],
                block_entities: vec![],
            });
            self.cache.insert(pos, handle);
        }
        self.chunk_map.remove_chunk(pos);
        if self.is_chunk_loading(pos) {
            self.canceled_chunk_loads.insert(pos);
        }

        log::trace!("Unloaded chunk {:?}", pos);
        Ok(())
    }

    /// Returns whether the given chunk is loaded.
    pub fn is_chunk_loaded(&self, pos: ChunkPosition) -> bool {
        self.chunk_map.0.contains_key(&pos)
    }

    /// Returns whether the given chunk is queued to be loaded.
    pub fn is_chunk_loading(&self, pos: ChunkPosition) -> bool {
        self.loading_chunks.contains(&pos)
    }

    /// Sets the block at the given position.
    ///
    /// Returns `true` if the block was set, or `false`
    /// if its chunk was not loaded or the coordinates
    /// are out of bounds and thus no operation
    /// was performed.
    pub fn set_block_at(&self, pos: BlockPosition, block: BlockId) -> bool {
        self.chunk_map.set_block_at(pos, block)
    }

    /// Retrieves the block at the specified
    /// location. If the chunk in which the block
    /// exists is not loaded or the coordinates
    /// are out of bounds, `None` is returned.
    pub fn block_at(&self, pos: BlockPosition) -> Option<BlockId> {
        self.chunk_map.block_at(pos)
    }

    pub fn adjacent_block_cubic(&self, pos: BlockPosition, dir: FacingCubic) -> Option<BlockId> {
        self.block_at(pos.adjacent(match dir {
            FacingCubic::North => BlockFace::North,
            FacingCubic::East => BlockFace::East,
            FacingCubic::South => BlockFace::South,
            FacingCubic::West => BlockFace::West,
            FacingCubic::Up => BlockFace::Top,
            FacingCubic::Down => BlockFace::Bottom,
        }))
    }

    pub fn adjacent_block_cardinal(
        &self,
        pos: BlockPosition,
        dir: FacingCardinal,
    ) -> Option<BlockId> {
        self.adjacent_block_cubic(pos, dir.to_facing_cubic())
    }

    pub fn adjacent_block_cardinal_and_down(
        &self,
        pos: BlockPosition,
        dir: FacingCardinalAndDown,
    ) -> Option<BlockId> {
        self.adjacent_block_cubic(pos, dir.to_facing_cubic())
    }

    pub fn get_facing_block(&self, pos: BlockPosition) -> Option<BlockId> {
        let block = self.block_at(pos)?;
        let a = block.facing_cardinal().map(FacingCardinal::to_facing_cubic);
        let b = block
            .facing_cardinal_and_down()
            .map(FacingCardinalAndDown::to_facing_cubic);
        let c = block.facing_cubic();
        let dir = [a, b, c].iter().find_map(|&e| e)?;
        self.adjacent_block_cubic(pos, dir)
    }

    pub fn set_block_adjacent_cubic(
        &self,
        pos: BlockPosition,
        block: BlockId,
        dir: FacingCubic,
    ) -> bool {
        self.set_block_at(
            pos.adjacent(match dir {
                FacingCubic::North => BlockFace::North,
                FacingCubic::East => BlockFace::East,
                FacingCubic::South => BlockFace::South,
                FacingCubic::West => BlockFace::West,
                FacingCubic::Up => BlockFace::Top,
                FacingCubic::Down => BlockFace::Bottom,
            }),
            block,
        )
    }

    pub fn set_block_adjacent_cardinal(
        &self,
        pos: BlockPosition,
        block: BlockId,
        dir: FacingCardinal,
    ) -> bool {
        self.set_block_adjacent_cubic(pos, block, dir.to_facing_cubic())
    }

    pub fn set_block_adjacent_cardinal_and_down(
        &self,
        pos: BlockPosition,
        block: BlockId,
        dir: FacingCardinalAndDown,
    ) -> bool {
        self.set_block_adjacent_cubic(pos, block, dir.to_facing_cubic())
    }

    pub fn check_block_stability(&self, block: BlockId, pos: BlockPosition) -> Option<bool> {
        Some(if let Some(support_type) = block.support_type() {
            use generated::SimplifiedBlockKind::*;
            let block_under = self.block_at(pos.down());
            let block_facing = self.get_facing_block(pos);
            match support_type {
                SupportType::OnSolid => block_under?.is_solid(),
                SupportType::OnDesertBlocks => matches!(
                    block_under?.simplified_kind(),
                    Sand | RedSand | Dirt | CoarseDirt | Podzol | Teracotta
                ),
                SupportType::OnDirtBlocks => matches!(
                    block_under?.simplified_kind(),
                    Dirt | GrassBlock | CoarseDirt | Podzol | Farmland
                ),
                SupportType::OnFarmland => block_under?.simplified_kind() == Farmland,
                SupportType::OnSoulSand => block_under?.simplified_kind() == SoulSand,
                SupportType::OnWater => block_under?.simplified_kind() == Water,
                SupportType::FacingSolid => block_facing?.is_solid(),
                SupportType::FacingJungleWood => matches!(
                    block_facing?.kind(),
                    BlockKind::JungleLog
                        | BlockKind::StrippedJungleLog
                        | BlockKind::JungleWood
                        | BlockKind::StrippedJungleWood
                ),
                SupportType::OnOrFacingSolid => true, // TODO: Everything
                SupportType::CactusLike => true,
                SupportType::ChorusFlowerLike => true,
                SupportType::ChorusPlantLike => true,
                SupportType::MushroomLike => true,
                SupportType::SnowLike => true,
                SupportType::SugarCaneLike => true,
                SupportType::TripwireHookLike => true,
                SupportType::VineLike => true,
            }
        } else {
            true
        })
    }

    /// Returns the chunk map.
    pub fn chunk_map(&self) -> &ChunkMap {
        &self.chunk_map
    }

    /// Mutably gets the chunk map.
    pub fn chunk_map_mut(&mut self) -> &mut ChunkMap {
        &mut self.chunk_map
    }

    pub fn load_player_data(&self, uuid: Uuid) -> anyhow::Result<PlayerData> {
        Ok(base::anvil::player::load_player_data(
            &self.world_dir,
            uuid,
        )?)
    }

    pub fn save_player_data(&self, uuid: Uuid, data: &PlayerData) -> anyhow::Result<()> {
        base::anvil::player::save_player_data(&self.world_dir, uuid, data)
    }
}

pub type ChunkMapInner = AHashMap<ChunkPosition, ChunkHandle>;

/// This struct stores all the chunks on the server,
/// so it allows access to blocks and lighting data.
///
/// Chunks are internally wrapped in `Arc<RwLock>`,
/// allowing multiple systems to access different parts
/// of the world in parallel. Mutable access to this
/// type is only required for inserting and removing
/// chunks.
#[derive(Default)]
pub struct ChunkMap(ChunkMapInner);

impl ChunkMap {
    /// Creates a new, empty world.
    pub fn new() -> Self {
        Self::default()
    }

    /// Retrieves a handle to the chunk at the given
    /// position, or `None` if it is not loaded.
    pub fn chunk_at(&self, pos: ChunkPosition) -> Option<RwLockReadGuard<Chunk>> {
        self.0.get(&pos).map(|lock| lock.read())
    }

    /// Retrieves a handle to the chunk at the given
    /// position, or `None` if it is not loaded.
    pub fn chunk_at_mut(&self, pos: ChunkPosition) -> Option<RwLockWriteGuard<Chunk>> {
        self.0.get(&pos).map(|lock| lock.write()).flatten()
    }

    /// Returns an `Arc<RwLock<Chunk>>` at the given position.
    pub fn chunk_handle_at(&self, pos: ChunkPosition) -> Option<ChunkHandle> {
        self.0.get(&pos).map(Arc::clone)
    }

    pub fn block_at(&self, pos: BlockPosition) -> Option<BlockId> {
        check_coords(pos)?;
        let (x, y, z) = chunk_relative_pos(pos);
        self.chunk_at(pos.into())
            .map(|chunk| chunk.block_at(x, y, z))
            .flatten()
    }

    pub fn set_block_at(&self, pos: BlockPosition, block: BlockId) -> bool {
        if check_coords(pos).is_none() {
            return false;
        }
        let (x, y, z) = chunk_relative_pos(pos);

        self.chunk_at_mut(pos.into())
            .map(|mut chunk| chunk.set_block_at(x, y, z, block))
            .is_some()
    }

    /// Returns an iterator over chunks.
    pub fn iter_chunks(&self) -> impl IntoIterator<Item = &ChunkHandle> {
        self.0.values()
    }

    /// Inserts a new chunk into the chunk map.
    pub fn insert_chunk(&mut self, chunk: Chunk) {
        self.0
            .insert(chunk.position(), Arc::new(ChunkLock::new(chunk, true)));
    }

    /// Removes the chunk at the given position, returning `true` if it existed.
    pub fn remove_chunk(&mut self, pos: ChunkPosition) -> bool {
        self.0.remove(&pos).is_some()
    }
}

fn check_coords(pos: BlockPosition) -> Option<()> {
    if pos.y >= 0 && pos.y < CHUNK_HEIGHT as i32 {
        Some(())
    } else {
        None
    }
}

fn chunk_relative_pos(block_pos: BlockPosition) -> (usize, usize, usize) {
    (
        block_pos.x as usize & 0xf,
        block_pos.y as usize,
        block_pos.z as usize & 0xf,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_out_of_bounds() {
        let mut world = World::new();
        world
            .chunk_map_mut()
            .insert_chunk(Chunk::new(ChunkPosition::new(0, 0)));

        assert!(world.block_at(BlockPosition::new(0, -1, 0)).is_none());
        assert!(world.block_at(BlockPosition::new(0, 0, 0)).is_some());
    }
}
