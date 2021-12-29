use base::{EntityKind, Position};
use ecs::{EntityBuilder, EntityRef, SysResult};
use quill_common::entity_init::EntityInit;
use uuid::Uuid;

use crate::{Client, NetworkId};

/// Component that sends the spawn packet for an entity
/// using its components.
pub struct SpawnPacketSender(fn(&EntityRef, &Client) -> SysResult);

impl SpawnPacketSender {
    pub fn send(&self, entity: &EntityRef, client: &Client) -> SysResult {
        (self.0)(entity, client)
    }
}

/// Stores the position of an entity on
/// the previous tick. Used to determine
/// when to send movement updates.
#[derive(Copy, Clone, Debug)]
pub struct PreviousPosition(pub Position);

pub fn add_entity_components(builder: &mut EntityBuilder, init: &EntityInit) {
    if !builder.has::<NetworkId>() {
        builder.add(NetworkId::new());
    }
    builder.add(PreviousPosition(*builder.get::<Position>().unwrap()));
    add_spawn_packet(builder, init);
}

fn add_spawn_packet(builder: &mut EntityBuilder, init: &EntityInit) {
    // TODO: object entities spawned with Spawn Entity
    // (minecarts, items, ...)
    let spawn_packet = match init {
        EntityInit::Player => spawn_player,
        _ => spawn_living_entity,
    };
    builder.add(SpawnPacketSender(spawn_packet));
}

fn spawn_player(entity: &EntityRef, client: &Client) -> SysResult {
    let network_id = *entity.get::<NetworkId>()?;
    let uuid = *entity.get::<Uuid>()?;
    let pos = *entity.get::<Position>()?;

    client.send_player(network_id, uuid, pos);
    Ok(())
}

fn spawn_living_entity(entity: &EntityRef, client: &Client) -> SysResult {
    let network_id = *entity.get::<NetworkId>()?;
    let uuid = *entity.get::<Uuid>()?;
    let pos = *entity.get::<Position>()?;
    let kind = *entity.get::<EntityKind>()?;

    client.send_living_entity(network_id, uuid, pos, kind);
    Ok(())
}
