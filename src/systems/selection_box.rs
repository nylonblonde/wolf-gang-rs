use gdnative::prelude::*;
use gdnative::api::{
    ImmediateGeometry,
    Spatial
};
use legion::*;
use nalgebra::Rotation3;
use num::Float;
use serde::{Serialize, Deserialize};

use std::cmp::Ordering;

use octree::geometry::aabb;

use crate::{
    editor,
    node,
    nodes::actor_palette::{
        ActorPalette,
        ENTITY_REFS,
    },
    systems::{
        actor,
        actor::{
            MERGER,
        },
        camera,
        custom_mesh,
        transform,
        input,
        level_map,
        networking::{ClientID, DataType, MessageSender, MessageType},
    }
};

type AABB = aabb::AABB<i32>;
type Point = nalgebra::Vector3<i32>;

type Vector3D = nalgebra::Vector3<f32>;
type Vector2D = nalgebra::Vector2<f32>;

#[derive(Copy, Clone)]
struct EntityRef(Entity);

#[derive(Copy, Clone, PartialEq)]
pub struct CameraAdjustedDirection {
    pub forward: Vector3D,
    pub right: Vector3D
}

impl Default for CameraAdjustedDirection {
    fn default() -> Self {
        CameraAdjustedDirection {
            forward: Vector3D::z(),
            right: Vector3D::x()
        }
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub enum ToolBoxType {
    TerrainToolBox,
    ActorToolBox(i64),
}

#[derive(Copy, Clone)]
/// TerrainToolBox is just a struct that is used as a way of tagging the selection box that should be visible and active while the tile tool is in use
pub struct TerrainToolBox {}

#[derive(Copy, Clone)]
/// ActorToolBox is just a struct that is used as away of tagging the selection box that should be visible and active while the actor placement tool is in use
pub struct ActorToolBox(i64);

impl ActorToolBox {
    pub fn get_selection(&self) -> i64 {
        self.0
    }
}

#[derive(Copy, Clone)]
/// Used to tag whichever selection box is active
pub struct Active {}

#[derive(Copy, Clone)]
/// Component pushed to world for activating the terrain tool box and sending the message to server
pub struct ActivateTerrainToolBox{}

#[derive(Copy, Clone)]
/// Component pushed to world for activating the actor tool box and sending the message to server
pub struct ActivateActorToolBox{}

#[derive(Copy, Clone)]
/// Componenet pushed to world to act on the chosen selection in actor palette and send the relevant message
pub struct MakeActorSelectionChosen{}

#[derive(Debug, Copy, Clone)]
pub struct SelectionBox {
    pub aabb: AABB
}

impl SelectionBox {
    ///Creates a SelectionBox with an aabb at center (0,0,0) with dimensions of (1,1,1).
    pub fn new() -> Self {
        SelectionBox {
            aabb: AABB::new(Point::new(0,0,0), Point::new(1,1,1))
        }
    }

    pub fn from_aabb(aabb: AABB) -> Self {
        SelectionBox {
            aabb
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub struct UpdateBounds {
    pub coord_pos: Point,
    pub aabb: AABB
}

#[derive(Debug, Copy, Clone)]
pub struct SelectionBoxRotation {
    pub value: Rotation3<f32>
}

#[derive(Copy, Clone)]
pub struct RelativeCamera(Ref<Node>);

impl RelativeCamera {
    pub fn val(&self) -> Ref<Node> {
        self.0
    }
}

/// Initializes and returns the entities for the different kinds of tool boxes
pub fn initialize_selection_box(world: &mut World, _: &mut Resources, client_id: u32, tool_type: ToolBoxType, camera_node: Option<Ref<Node>>) -> Entity {

    // TerrainTool selection box
    let mesh: Ref<ImmediateGeometry, Unique> = ImmediateGeometry::new();
    mesh.set_visible(false);

    let owner = unsafe { crate::OWNER_NODE.as_mut().unwrap().assume_safe() };

    let node = unsafe { node::add_node(&owner, mesh.upcast()) };
    
    match tool_type {
        ToolBoxType::TerrainToolBox => {
            let entity = world.push(
                (
                    node::NodeRef::new(node),
                    ClientID::new(client_id),
                    SelectionBox::new(),
                    custom_mesh::MeshData::new(),
                    level_map::CoordPos::default(),
                    transform::position::Position::default(), 
                    CameraAdjustedDirection::default(),
                    custom_mesh::Material::from_str("res://materials/select_box.material")
                )
            );
        
            if let Some(mut entry) = world.entry(entity) {
                entry.add_component(TerrainToolBox{});
        
                if let Some(camera_node) = camera_node {
                    entry.add_component(RelativeCamera(camera_node))
                }
            }

            entity
        },
        ToolBoxType::ActorToolBox(actor_id) => {
            let entity = world.push(
                (
                    node::NodeRef::new(node),
                    ClientID::new(client_id),
                    custom_mesh::MeshData::new(),
                    level_map::CoordPos::default(),
                    transform::position::Position::default(),
                    SelectionBoxRotation{
                        value: Rotation3::identity()
                    },
                    CameraAdjustedDirection::default(),
                    custom_mesh::Material::from_str("res://materials/select_box.material")
                )
            );
        
            //have to add extra components via entry because world.push can only take 8
            if let Some(mut entry) = world.entry(entity) {
                entry.add_component(SelectionBox::new());
                entry.add_component(ActorToolBox(actor_id));
        
                if let Some(camera_node) = camera_node {
                    entry.add_component(RelativeCamera(camera_node))
                }
            }
        
            entity
        }
    }

}

/// Removes all SelectionBox entities from the world, and frees and removes the related Godot nodes
pub fn free_all(world: &mut World) {
    let mut selection_box_query = <Read<node::NodeRef>>::query()
        .filter(component::<SelectionBox>());

    selection_box_query.iter(world)
        .map(|node_ref| node_ref.val())
        .collect::<Vec<Ref<Node>>>()
        .into_iter()
        .for_each(|node_ref| {
            node::free(world, node_ref);
        });
}

/// Gets the axis closest to forward from a or b, adjusted by adjust_angle around the up axis. We adjust it so that we can smooth out the comparison at 45
/// degree angles.
fn get_forward_closest_axis(a: &Vector3D, b: &Vector3D, forward: &Vector3D, right: &Vector3D, up: &nalgebra::Unit<Vector3D>, adjust_angle: f32) -> std::cmp::Ordering {
    
    let a_dot = a.dot(right);
    let b_dot = b.dot(right);

    let dot = match PartialOrd::partial_cmp(&a_dot, &b_dot) {
        None => 0., //If NaN just set it to 0
        Some(Ordering::Less) => a_dot,
        Some(_) => b_dot
    };

    let dir = match PartialOrd::partial_cmp(&dot, &0.) {
        None => 0., //If NaN just set it to 0
        Some(Ordering::Less) => -1.,
        Some(_) => 1.
    };

    let forward = nalgebra::UnitQuaternion::<f32>::from_axis_angle(up, adjust_angle*dir) * forward;

    a.dot(&forward).partial_cmp(
        &b.dot(&forward)
    ).unwrap()
}

/// System that keeps track of and swaps out the selected actor for the actor tool
pub fn create_actor_selection_chooser_system() -> Box<dyn FnMut(&mut World, &mut Resources)> {

    let mut selection_box_query = <(Entity, Read<ClientID>)>::query()
        .filter(component::<SelectionBox>() & component::<node::NodeRef>() & component::<ActorToolBox>() & component::<SelectionBoxRotation>());
    let mut query = <(Entity, Read<MakeActorSelectionChosen>)>::query();

    Box::new(move |world, resources| {

        let results = query.iter(world)
            .map(|(entity, _)| *entity)
            .collect::<Vec<Entity>>();

        if results.get(0).is_some() {

            if let Some(actor_selection) = resources.get::<editor::ActorPaletteSelection>() {
                if let Some(client_id) = resources.get::<ClientID>() {

                    selection_box_query.iter(world)
                        .filter(|(_, id)| id.val() == client_id.val())
                        .map(|(entity, _)| *entity)
                        .collect::<Vec<Entity>>()
                        .into_iter()
                        .for_each(|selection_entity| {

                            update_chosen_actor(world, selection_entity, actor_selection.val());

                            world.push(
                                (
                                    MessageSender{
                                        data_type: DataType::ActorToolSelection {
                                            client_id: client_id.val(),
                                            actor_id: actor_selection.val(),
                                        },
                                        message_type: MessageType::Ordered,
                                    },
                                )
                            );
                                            
                        });
                }
            }
        }

        results.into_iter()
            .for_each(|entity| {
                world.remove(entity);
            });
    })
}

/// System for sending the ActivateTerrainToolBox Message
/// We do this because we need access to ClientID before we can send the message, so handling it through a system helps guarantee that
pub fn create_terrain_tool_activate_system() -> impl systems::Runnable {
    SystemBuilder::new("terrain_tool_activate_message_sending_system")
        .read_resource::<ClientID>()
        .with_query(<Read<SelectionBox>>::query())
        .with_query(<(Entity, Read<ActivateTerrainToolBox>)>::query())
        .build(move |command, world, client_id, (selection_box_query, query)| {

            //kinda hacky, but we can ensure this never runs if connection hasn't been established and selection boxes haven't initialized
            if selection_box_query.iter(world).next().is_none() {
                return
            }
            
            let client_id = **client_id;
            for (entity, _) in query.iter(world) {

                let entity = *entity;

                command.exec_mut(move |world, _| {

                    set_active_selection_box::<TerrainToolBox>(world, client_id);

                    world.push(
                        (MessageSender{
                            data_type: DataType::ActivateTerrainToolBox{
                                client_id: client_id.val()
                            },
                            message_type: MessageType::Ordered
                        },)
                    );
                    world.remove(entity);

                });
            }
        })
}

/// System for sending the ActivateActorToolBox Message
pub fn create_actor_tool_activate_system() -> impl systems::Runnable {
    SystemBuilder::new("actor_tool_activate_message_sending_system")
        .read_resource::<ClientID>()
        .with_query(<Read<SelectionBox>>::query())
        .with_query(<(Entity, Read<ActivateActorToolBox>)>::query())
        .build(move |command, world, client_id, (selection_box_query, query)| {

            //kinda hacky, but we can ensure this never runs if connection hasn't been established and selection boxes haven't initialized
            if selection_box_query.iter(world).next().is_none() {
                return
            }
            
            let client_id = **client_id;
            for (entity, _) in query.iter(world) {
                command.exec_mut(move |world, _| {
                    set_active_selection_box::<ActorToolBox>(world, client_id);

                    world.push(
                        (MessageSender{
                            data_type: DataType::ActivateActorToolBox{
                                client_id: client_id.val()
                            },
                            message_type: MessageType::Ordered
                        },)
                    );
                });
                command.remove(*entity);
            }
        })
}

/// Calculates the orthogonal direction that should be considered forward and right when grid-like directional input is used.
pub fn create_orthogonal_dir_system() -> impl systems::Runnable {

    SystemBuilder::new("orthogonal_dir_system")
        .with_query(<(Write<CameraAdjustedDirection>, Read<RelativeCamera>)>::query())
        .with_query(<(Read<transform::rotation::Direction>, Read<node::NodeRef>)>::query()
            .filter(maybe_changed::<transform::rotation::Direction>() & component::<camera::FocalPoint>()))
        .build(|_, world, _, queries| {

            let (selection_box_query, cam_query) = queries;

            let cameras = cam_query.iter(world)
                .map(|(dir, node_ref)| (*dir, node_ref.val()))
                .collect::<Vec<(transform::rotation::Direction, Ref<Node>)>>();

            for (mut camera_adjusted_dir, relative_cam) in selection_box_query.iter_mut(world) {

                if let Some((dir, _)) = cameras.iter().find(|(_,node)| *node == relative_cam.0) {

                    // Get whichever cartesian direction in the grid is going to act as "forward" based on its closeness to the camera's forward
                    // view.
                    let mut forward = dir.forward;
                    let mut right = dir.right;

                    forward.y = 0.;
                    
                    let adjustment_angle = std::f32::consts::FRAC_PI_8;

                    forward = std::cmp::min_by(Vector3D::z(), 
                        std::cmp::min_by(-Vector3D::z(), 
                            std::cmp::min_by(Vector3D::x(), -Vector3D::x(),
                                |lh: &Vector3D, rh: &Vector3D| {
                                    get_forward_closest_axis(lh, rh, &forward, &right, &Vector3D::y_axis(), adjustment_angle)
                                }
                            ), 
                            |lh: &Vector3D, rh: &Vector3D| {
                                get_forward_closest_axis(lh, rh, &forward, &right, &Vector3D::y_axis(), adjustment_angle)
                            }
                        ), 
                        |lh: &Vector3D, rh: &Vector3D| {
                            get_forward_closest_axis(lh, rh, &forward, &right, &Vector3D::y_axis(), adjustment_angle)
                        }
                    );

                    //calculate right from up and forward by just rotating forward by -90 degrees
                    right =  nalgebra::UnitQuaternion::<f32>::from_axis_angle(&Vector3D::y_axis(), -std::f32::consts::FRAC_PI_2) * forward;

                    forward = forward.normalize();
                    right = right.normalize();

                    camera_adjusted_dir.forward = forward;
                    camera_adjusted_dir.right = right;
                
                }
            }
    })
} 

/// This system reads input, then moves the coord position of the selection_box
pub fn create_movement_system() -> impl systems::Runnable {
    
    let move_forward = input::Action("move_forward".to_string());
    let move_back = input::Action("move_back".to_string());
    let move_left = input::Action("move_left".to_string());
    let move_right = input::Action("move_right".to_string());
    let move_up = input::Action("move_up".to_string());
    let move_down = input::Action("move_down".to_string());

    SystemBuilder::new("selection_box_movement_system")
        .read_resource::<crate::Time>()
        .read_resource::<ClientID>()
        .with_query(<(Read<input::InputActionComponent>, Read<input::Action>)>::query())
        .with_query(<(Read<CameraAdjustedDirection>, Read<ClientID>, Read<level_map::CoordPos>, Read<SelectionBox>)>::query())
        .build(move |commands, world, (time, client_id), queries| {

            let (input_query, selection_box_query) = queries;

            let inputs = input_query.iter(world)
                .map(|(input, action)| (*input, (*action).clone()))
                .collect::<Vec<(input::InputActionComponent, input::Action)>>();

            let mut combined_movement: Option<Point> = None;
            let mut entity: Option<(Point, ClientID, SelectionBox)> = None;

            for(input_component, action) in inputs.iter().filter(|(_, a)|
                a == &move_forward ||
                a == &move_back ||
                a == &move_left ||
                a == &move_right ||
                a == &move_up ||
                a == &move_down
            ) {                    

                if input_component.repeated(time.delta, 0.25) {

                    selection_box_query.iter(world)
                        .filter(|(_, id, _, _)| **id == **client_id)
                        .for_each(|(camera_adjusted_dir, _, coord_pos, selection_box)| {

                        entity = Some((coord_pos.value, **client_id, *selection_box));

                        let mut movement = Point::zeros();

                        if action.0 == move_forward.0 {
                            movement.z += 1;
                        } else if action.0 == move_back.0 {
                            movement.z -= 1;
                        } else if action.0 == move_left.0 {
                            movement.x -= 1;
                        } else if action.0 == move_right.0 {
                            movement.x += 1;
                        } else if action.0 == move_up.0 {
                            movement.y += 1;
                        } else if action.0 == move_down.0 {
                            movement.y -= 1;
                        }
                        
                        let forward = camera_adjusted_dir.forward;
                        let right = camera_adjusted_dir.right;

                        let mut adjusted = Point::new(
                            forward.x.round() as i32,
                            0,
                            forward.z.round() as i32
                        ) * movement.z + Point::new(
                            right.x.round() as i32,
                            0,
                            right.z.round() as i32
                        ) * movement.x;

                        adjusted.y = movement.y;

                        combined_movement = Some(adjusted);

                    });
                }
            }   
            
            if let Some(combined_movement) = combined_movement {
                if let Some((coord_pos_value, client_id, selection_box)) = entity {

                    let move_to_pos = coord_pos_value + combined_movement;

                    commands.exec_mut(move |world, _| {
                        let mut query = <(Write<UpdateBounds>, Read<ClientID>)>::query();

                        let mut existing_movement: Option<Point> = None;

                        if let Some((update_to, _)) = query.iter_mut(world).find(|(_, id)| **id == client_id) {
                            update_to.coord_pos += combined_movement;
                            existing_movement = Some(update_to.coord_pos);
                        }

                        let mut update_selection = DataType::UpdateSelectionBounds{ client_id: client_id.val(), coord_pos: move_to_pos, aabb: selection_box.aabb };

                        match existing_movement {
                            Some(existing_movement) => {
                                if let DataType::UpdateSelectionBounds{client_id:_, coord_pos, aabb:_} = &mut update_selection {
                                    *coord_pos = existing_movement;
                                } 
                            },
                            None => {
                                if let DataType::UpdateSelectionBounds{client_id:_, coord_pos, aabb} = &mut update_selection {
                                    world.push(
                                        (
                                            UpdateBounds {
                                                aabb: *aabb,
                                                coord_pos: *coord_pos
                                            },
                                            client_id
                                        )
                                    );
                                }
                            }
                        }

                        world.push((MessageSender{
                            data_type: update_selection,
                            message_type: MessageType::Ordered
                        },));
                    });
                }
            }
        })
}

pub fn create_coord_to_pos_system() -> impl systems::Runnable {
    SystemBuilder::new("selection_box_coord_system")
        .with_query(<(Read<level_map::CoordPos>, Write<transform::position::Position>,)>::query()
            .filter(maybe_changed::<level_map::CoordPos>() & component::<SelectionBox>())
        )
        .build(move |_, world, _, query| {

            query.for_each_mut(world, |(coord_pos, mut position)| {
                position.value = level_map::map_coords_to_world(coord_pos.value); 
            })
        })
}

/// The system responsible for the actor tool functions, such as insertion, removal, moving, editing, etc
pub fn create_actor_tool_system() -> impl systems::Runnable {
    let insertion = input::Action(("insertion").to_string());
    let removal = input::Action(("removal").to_string());

    SystemBuilder::new("actor_tool_system")
        .read_resource::<ClientID>()
        // .read_resource::<editor::ActorPaletteSelection>()
        .with_query(<(Read<SelectionBox>, Read<level_map::CoordPos>, Read<EntityRef>, Read<ClientID>)>::query() 
            .filter(component::<ActorToolBox>() & component::<Active>()))
        .with_query(<(Read<input::InputActionComponent>, Read<input::Action>)>::query())
        .build(move |command, world, resources, queries| {
            let (selection_box_query, input_query) = queries;
            let client_id = resources;

            input_query.iter(world).filter(|(_, a)| {
                *a == &insertion || *a == &removal
            }).for_each(|(input_component, action)|  {
                // Insertion tool should check whether or not this is a valid placement for the actor
                selection_box_query.iter(world).filter(|(_, _, _, id)| **id == **client_id).for_each(|(selection_box, coord_pos, entity_ref, _)| {

                    if input_component.just_pressed() {

                        if action == &insertion {
                            
                            let client_id = client_id.val();
                            let coord_pos = *coord_pos;
                            let actor_entity = entity_ref.0;

                            command.exec_mut(move |world, _| {

                                actor::CANON.with(move |c| {
                                    let canon = c.borrow();

                                    actor::REGISTRY.with(move |r| {
                                        let registry = r.borrow();

                                        actor::MERGER.with(move |m| {

                                            let mut merger = m.borrow_mut();

                                            let mut actor_world = World::default();
                                            let new_entity = actor_world.clone_from_single(world, actor_entity, &mut *merger);

                                            if let Some(mut entry) = actor_world.entry(new_entity) {
                                                let actor_id = actor::ActorID::new();
                                                entry.add_component(actor_id);
                                                entry.add_component(coord_pos);
                                            }
                                            
                                            if let Ok(serialized) = bincode::serialize(&actor_world.as_serializable(component::<actor::Actor>(), & *registry, & *canon)) {
                                                world.push(
                                                    (
                                                        MessageSender{
                                                            data_type: DataType::ActorChange{
                                                                store_history: Some(client_id),
                                                                change: actor::ActorChange::ActorInsertion {
                                                                    serialized
                                                                },
                                                            },
                                                            message_type: MessageType::Ordered,
                                                        },
                                                    )
                                                );
                                            }
                                            
                                        });
                                    });
                                });
                            });

                        } else if action == &removal {
                            
                            let coord_pos = coord_pos.value;
                            let dimensions = selection_box.aabb.dimensions;
                            let client_id = client_id.val();
                            command.exec_mut(move |world, _| {
                                actor::select_actors_from_range(world, AABB::new(coord_pos, dimensions))
                                    .into_iter().for_each(|entity| {
                                        if let Some(Some(actor_id)) = world.entry(entity).map(|entry| {
                                                entry.get_component::<actor::ActorID>().ok().copied()
                                            }
                                        ) {
                                            world.push(
                                                (
                                                    MessageSender{
                                                        data_type: DataType::ActorChange {
                                                            change: actor::ActorChange::ActorRemoval(actor_id.val()),
                                                            store_history: Some(client_id)
                                                        },
                                                        message_type: MessageType::Ordered
                                                    },
                                                )
                                            );
                                        }
                                    });
                            })
                            
                        }
                    }
                })
            })
        })
}

/// The system responsible for the tile tool functions, such as insertion, removal, and (to be added) copy, paste, painting
pub fn create_tile_tool_system() -> impl systems::Runnable {
    let insertion = input::Action(("insertion").to_string());
    let removal = input::Action(("removal").to_string());

    SystemBuilder::new("tile_tool_system")
        .read_resource::<ClientID>()
        .read_resource::<level_map::Map>()
        .read_resource::<editor::PaletteSelection>()
        .with_query(<(Read<SelectionBox>, Read<level_map::CoordPos>, Read<ClientID>)>::query() //all selection_boxes
            .filter(component::<TerrainToolBox>() & component::<Active>()))
        .with_query(<(Read<SelectionBox>, Read<level_map::CoordPos>, Read<ClientID>)>::query() //only moved selection_boxes
            .filter(component::<TerrainToolBox>() & component::<Active>() & maybe_changed::<level_map::CoordPos>()))
        .with_query(<(Read<input::InputActionComponent>, Read<input::Action>)>::query())
        .build(move |commands, world, resources, queries| {

            let (selection_box_query, selection_box_moved_query, input_query) = queries;
            let (client_id, map, tile_selection) = resources;

            input_query.iter(world).filter(|(_, a)| {
                *a == &insertion || *a == &removal
            }).for_each(|(input_component, action)|  {
                selection_box_query.iter(world).filter(|(_, _, id)| id.val() == client_id.val()).for_each(|(selection_box, coord_pos, _)| {
                    
                    let moved = selection_box_moved_query.iter(world).any(|(_, _, id)| id.val() == client_id.val());

                    if input_component.just_pressed() 
                    || (input_component.is_held() && moved) 
                    {
                        if action == &insertion {
                            let map = **map;
                            let tile_selection = **tile_selection;

                            let client_id = client_id.val();
                            let aabb = AABB::new(coord_pos.value, selection_box.aabb.dimensions);

                            commands.exec_mut(move |world, _|{
                
                                let tile_data = level_map::TileData::new(tile_selection.val(), Point::zeros());
            
                                if map.can_change(world, &level_map::fill_octree_from_aabb(aabb, Some(tile_data))).is_ok() {
                                    world.push(
                                        (
                                            MessageSender{
                                                data_type: DataType::MapChange{
                                                    store_history: Some(client_id),
                                                    change: level_map::MapChange::MapInsertion{ aabb, tile_data },                               
                                                },
                                                message_type: MessageType::Ordered
                                            },
                                        ),                  
                                    );
                                }
                            });

                        } else if action == &removal {
                            let map = **map;
                            let client_id = client_id.val();
                            let aabb = AABB::new(coord_pos.value, selection_box.aabb.dimensions);

                            commands.exec_mut(move |world, _|{
                                if map.can_change(world, &level_map::fill_octree_from_aabb(aabb, None)).is_ok() {
                                    world.push(
                                        (
                                            MessageSender{
                                                data_type: DataType::MapChange{
                                                    store_history: Some(client_id),
                                                    change: level_map::MapChange::MapRemoval(aabb),                               
                                                },
                                                message_type: MessageType::Ordered
                                            },
                                        ),                  
                                    );
                                }
                            });
                        }
                        
                    }
                })
            })
        })
}

pub fn create_rotation_system() -> impl systems::Runnable {
    let rotate_selection_left = input::Action("rotate_selection_left".to_string());
    let rotate_selection_right = input::Action("rotate_selection_right".to_string());

    SystemBuilder::new("selection_rotation_system")
        .read_resource::<crate::Time>()
        .read_resource::<ClientID>()
        .with_query(<(Read<input::InputActionComponent>, Read<input::Action>)>::query())
        .with_query(<(Entity, Read<ClientID>)>::query()
            .filter(component::<SelectionBox>() & component::<ActorToolBox>() & component::<Active>()))
        .build(move |commands, world, (time, client_id), queries| {
            let (input_query, selection_box_query) = queries;

            let inputs = input_query.iter(world)
                .map(|(input, action)| (*input, (*action).clone()))
                .collect::<Vec<(input::InputActionComponent, input::Action)>>();

            inputs.into_iter()
                .filter(|(_, a)|
                    a == &rotate_selection_left
                    || a == &rotate_selection_right
                )
                .for_each(|(input_component, action)| {
                    if input_component.repeated(time.delta, 0.25) {

                        selection_box_query.iter_mut(world)
                            .filter(|(_, id)| id.val() == client_id.val())
                            .for_each(|(entity, _)| {

                                let rotation = if action == rotate_selection_left {
                                    Rotation3::from_axis_angle(&Vector3D::y_axis(), std::f32::consts::FRAC_PI_2)
                                } else if action == rotate_selection_right {
                                    Rotation3::from_axis_angle(&Vector3D::y_axis(), -std::f32::consts::FRAC_PI_2)
                                } else {
                                    Rotation3::identity()
                                };

                                let entity = *entity;
                                let client_id = client_id.val();

                                commands.exec_mut(move |world, _| {
                                    actor_tool_rotation(world, entity, rotation);

                                    world.push(
                                        (MessageSender{
                                            data_type: DataType::ActorToolRotation {
                                                client_id,
                                                rotation
                                            },
                                            message_type: MessageType::Ordered
                                        },)
                                    );
                                });

                            });

                    }
                });
        })
}

/// Expands the dimensions of the selection box
pub fn create_expansion_system() -> impl systems::Runnable {    

    let expand_selection_forward = input::Action("expand_selection_forward".to_string());
    let expand_selection_back = input::Action("expand_selection_back".to_string());
    let expand_selection_left = input::Action("expand_selection_left".to_string());
    let expand_selection_right = input::Action("expand_selection_right".to_string());
    let expand_selection_up = input::Action("expand_selection_up".to_string());
    let expand_selection_down = input::Action("expand_selection_down".to_string());

    SystemBuilder::new("selection_expansion_system")
        .read_resource::<crate::Time>()
        .read_resource::<ClientID>()
        .with_query(<(Read<input::InputActionComponent>, Read<input::Action>)>::query())
        .with_query(<(Read<CameraAdjustedDirection>, Read<ClientID>, Read<level_map::CoordPos>, Read<SelectionBox>)>::query()
            .filter(component::<TerrainToolBox>() & component::<Active>()))
        .build(move |commands, world, (time, client_id), queries| {
            let (input_query, selection_box_query) = queries;

            let inputs = input_query.iter(world)
                .map(|(input, action)| (*input, (*action).clone()))
                .collect::<Vec<(input::InputActionComponent, input::Action)>>();

            //left: movement, right: expansion
            let mut combined_expansion: Option<Point> = None;
            let mut entity: Option<(CameraAdjustedDirection, Point, AABB, ClientID)> = None;

            for(input_component, action) in inputs.iter().filter(|(_, a)|
                a == &expand_selection_forward ||
                a == &expand_selection_back ||
                a == &expand_selection_left ||
                a == &expand_selection_right ||
                a == &expand_selection_up ||
                a == &expand_selection_down
            ) {                    
                
                if input_component.repeated(time.delta, 0.25) {

                    selection_box_query.iter(world)
                        .filter(|(_, id, _, _)| **id == **client_id)
                        .for_each(|(camera_adjusted_dir, client_id, coord_pos, selection_box)| {

                        entity = Some((*camera_adjusted_dir, coord_pos.value, selection_box.aabb, *client_id));

                        let mut expansion = Point::zeros();

                        if action == &expand_selection_forward {
                            expansion.z += 1;
                        } else if action == &expand_selection_back {
                            expansion.z -= 1;
                        } else if action == &expand_selection_left {
                            expansion.x -= 1;
                        } else if action == &expand_selection_right {
                            expansion.x += 1;
                        } else if action == &expand_selection_down {
                            expansion.y -= 1;
                        } else if action == &expand_selection_up {
                            expansion.y += 1;
                        }

                        let forward = camera_adjusted_dir.forward;
                        let right = camera_adjusted_dir.right;

                        let mut adjusted = Point::new(
                            forward.x.round().abs() as i32,
                            0,
                            forward.z.round().abs() as i32
                        ) * expansion.z as i32 + Point::new(
                            right.x.round().abs() as i32,
                            0,
                            right.z.round().abs() as i32
                        ) * expansion.x as i32;

                        adjusted.y = expansion.y as i32;

                        combined_expansion = Some(adjusted);

                    }); 
                }
            }

            if let Some(combined_expansion) = combined_expansion {
                if let Some((camera_adjusted_dir, coord_pos_value, aabb, client_id)) = entity {
                    
                    commands.exec_mut(move |world, _| {
                        let mut query = <(Write<UpdateBounds>, Read<ClientID>)>::query();

                        let mut existing_expansion: Option<(Point, AABB)> = None;

                        let mut new_aabb = aabb;
                                    
                        let diff = expansion_movement_helper(combined_expansion, camera_adjusted_dir, &mut new_aabb);

                        let move_to_pos = coord_pos_value - diff;

                        if let Some((update_to, _)) = query.iter_mut(world).find(|(_, id)| **id == client_id) {
                            
                            update_to.coord_pos -= diff;
                            update_to.aabb.dimensions += combined_expansion;

                            existing_expansion = Some((update_to.coord_pos, update_to.aabb));
                        }

                        let mut update_selection = DataType::UpdateSelectionBounds{ client_id: client_id.val(), coord_pos: move_to_pos, aabb: new_aabb };

                        match existing_expansion {
                            Some(existing_expansion) => {
                                if let DataType::UpdateSelectionBounds{client_id:_, coord_pos, aabb} = &mut update_selection {

                                    *coord_pos = existing_expansion.0;
                                    *aabb = existing_expansion.1;
                                }
                            },
                            None => {
                                if let DataType::UpdateSelectionBounds{client_id:_, coord_pos, aabb} = &mut update_selection {
                                    world.push(
                                        (
                                            UpdateBounds {
                                                aabb: *aabb,
                                                coord_pos: *coord_pos
                                            },
                                            client_id
                                        )
                                    );
                                }
                            }
                        }

                        world.push((MessageSender{
                            data_type: update_selection,
                            message_type: MessageType::Ordered
                        },));

                    });
                }
            }  
        })
}

pub fn create_update_bounds_system() -> impl systems::Runnable {
    SystemBuilder::new("selection_box_move_to_system")
        .with_query(<(Entity, Read<ClientID>, Read<SelectionBox>)>::query())
        .with_query(<(Entity, Read<ClientID>, Read<UpdateBounds>)>::query())
        .build(|commands, world, _, queries| {
            let (selection_box_query, move_to_query) = queries;

            let move_tos = move_to_query.iter(world)
                .map(|(entity, client_id, update_to)| (*entity, *client_id, *update_to))
                .collect::<Vec<(Entity, ClientID, UpdateBounds)>>();

            selection_box_query.for_each(world, |(entity, client_id, selection_box)| {

                if let Some((update_entity, _, update_to)) = move_tos.iter().find(|(_,id,_)| id == client_id) {
                    
                    let update_entity = *update_entity;
                    let entity = *entity;
                    let update_to = *update_to;
                    let selection_box = *selection_box;

                    commands.exec_mut(move |world, _|{

                        if let Some(mut entry) = world.entry(entity) {
                            if let Ok(coord_pos) = entry.get_component_mut::<level_map::CoordPos>() {
                                coord_pos.value = update_to.coord_pos;
                            }

                            if selection_box.aabb != update_to.aabb { //only write to SelectionBox if there is an actual change
                                if entry.get_component::<Active>().is_ok() { //only update bounds if this is the active toolbox
                                    if let Ok(selection_box) = entry.get_component_mut::<SelectionBox>() {
                                        selection_box.aabb = update_to.aabb;
                                    }
                                }
                            }
                        }

                        world.remove(update_entity);
                    });

                }
            });
        })
}

pub fn create_system() -> impl systems::Runnable {
    
    SystemBuilder::new("selection_box_system")
        .with_query(<(Read<SelectionBox>, Write<custom_mesh::MeshData>,)>::query()
            .filter(maybe_changed::<SelectionBox>(),)
        )
        .build(move |_, world, _, query| {

            query.for_each_mut(world, |(selection_box, mesh_data)| {

                mesh_data.verts.clear();
                mesh_data.normals.clear();
                mesh_data.uvs.clear();
                mesh_data.indices.clear();

                //offset that the next face will begin on, increments by the number of verts for each face
                //at the end of each loop
                let mut offset = 0;

                let center = level_map::map_coords_to_world(selection_box.aabb.center);

                let min = level_map::map_coords_to_world(selection_box.aabb.get_min()) - center;
                let max = level_map::map_coords_to_world(selection_box.aabb.get_max() + Point::new(1,1,1)) - center;

                let true_center = (max + min) / 2.0;
                let true_dimensions = level_map::map_coords_to_world(selection_box.aabb.dimensions);

                let abs_dimensions = Vector3D::new(
                    true_dimensions.x.abs(),
                    true_dimensions.y.abs(),
                    true_dimensions.z.abs()
                );

                for i in 0..3 { 

                    let mut verts: Vec<Vector3> = Vec::new();  
                    let mut normals: Vec<Vector3> = Vec::new();
                    let mut uvs: Vec<Vector2> = Vec::new();

                    let max_margin = 0.9;

                    let smaller_x = Float::min(max_margin, abs_dimensions.x /2.0);
                    let smaller_y = Float::min(max_margin, abs_dimensions.y /2.0);
                    let smaller_z = Float::min(max_margin, abs_dimensions.z /2.0);

                    let margin = Float::min(smaller_x, Float::min(smaller_y, smaller_z));

                    match i {
                        0 => { // top and bottom

                            //store vectors as nalgebra's Vector3 to do transformations
                            let mut pts: Vec<Vector3D> = Vec::new();

                            let top_right = Vector3D::new(max.x , max.y , max.z );
                            let inner_top_right = Vector3D::new( //inner top right
                                max.x  - margin,
                                max.y ,
                                max.z  - margin
                            );

                            pts.push(Vector3D::new(min.x , max.y , max.z )); //0 top left
                            pts.push(top_right); //1
                            pts.push(Vector3D::new( //2 inner top left
                                min.x  + margin,
                                max.y ,
                                max.z  - margin
                            ));
                            pts.push(inner_top_right); //3
                            pts.push(top_right); //4
                            pts.push(Vector3D::new(max.x , max.y , min.z )); //5 bottom right
                            pts.push(inner_top_right); //6
                            pts.push(Vector3D::new( //7 inner bottom right
                                    max.x  - margin,
                                    max.y ,
                                    min.z  + margin
                            ));

                            let mut uv: Vec<Vector2D> = Vec::new();

                            uv.push(Vector2D::new(0.0, 0.0));
                            uv.push(Vector2D::new(1.0 * abs_dimensions.x , 0.0));
                            uv.push(Vector2D::new(margin, margin));
                            uv.push(Vector2D::new(1.0 * abs_dimensions.x - margin, margin));

                            uv.push(Vector2D::new(0.0, 0.0));
                            uv.push(Vector2D::new(1.0 * abs_dimensions.z , 0.0));
                            uv.push(Vector2D::new(margin, margin));
                            uv.push(Vector2D::new(1.0 * abs_dimensions.z - margin, margin));

                            for (pt, u) in pts.iter().zip(uv.iter()) {

                                uvs.push(Vector2::new(u.x, u.y));
                                verts.push(Vector3::new(pt.x, pt.y, pt.z));
                            }

                            let pts_len = pts.len();
                            for i in 0..pts_len {

                                let new_pt = pts[i] - true_center;
                                let u = uv[i];

                                let rot = Rotation3::new(Vector3D::y() * std::f32::consts::PI);
                                let rotated_pt = rot.transform_vector(&new_pt) + true_center;

                                pts.push(rotated_pt);
                                uv.push(u);

                                uvs.push(Vector2::new(u.x, u.y));
                                verts.push(Vector3::new(rotated_pt.x, rotated_pt.y, rotated_pt.z));
                            }

                            for _ in 0..pts.len() {
                                normals.push(Vector3::new(0.0, 1.0, 0.0));
                            }

                            for (pt, u) in pts.iter().zip(uv.iter()) {
                                let new_pt = pt - true_center;

                                let rot = Rotation3::new(Vector3D::x() * std::f32::consts::PI);
                                let rotated_pt = rot.transform_vector(&new_pt) + true_center;

                                uvs.push(Vector2::new(u.x, u.y));
                                verts.push(Vector3::new(rotated_pt.x, rotated_pt.y, rotated_pt.z));
                                normals.push(Vector3::new(0.0,-1.0,0.0));
                            }

                        },
                        1 => { //left and right

                            let mut pts: Vec<Vector3D> = Vec::new();

                            let top_right = Vector3D::new(max.x , max.y , max.z );
                            let inner_top_right = Vector3D::new( //inner top right
                                max.x ,
                                max.y  - margin,
                                max.z  - margin
                            );

                            pts.push(Vector3D::new(max.x , max.y , min.z )); //0 top left
                            pts.push(top_right); //1
                            pts.push(Vector3D::new( //2 inner top left
                                max.x ,
                                max.y  - margin,
                                min.z  + margin
                            ));
                            pts.push(inner_top_right); //3
                            pts.push(top_right); //4
                            pts.push(Vector3D::new(max.x , min.y , max.z )); //5 bottom right
                            pts.push(inner_top_right); //6
                            pts.push(Vector3D::new( //7 inner bottom right
                                max.x ,
                                min.y  + margin,
                                max.z  - margin
                            ));

                            let mut uv: Vec<Vector2D> = Vec::new();

                            uv.push(Vector2D::new(1.0 * abs_dimensions.z , 0.0));
                            uv.push(Vector2D::new(0.0, 0.0));
                            uv.push(Vector2D::new(1.0 * abs_dimensions.z  - margin, margin));
                            uv.push(Vector2D::new(margin, margin));

                            uv.push(Vector2D::new(1.0 * abs_dimensions.y , 0.0));
                            uv.push(Vector2D::new(0.0, 0.0));
                            uv.push(Vector2D::new(1.0 * abs_dimensions.y  - margin, margin));
                            uv.push(Vector2D::new(margin, margin));

                            for (pt, u) in pts.iter().zip(uv.iter()) {

                                uvs.push(Vector2::new(u.x, u.y));
                                verts.push(Vector3::new(pt.x, pt.y, pt.z));
                            }

                            let pts_len = pts.len();
                            for i in 0..pts_len {

                                let new_pt = pts[i] - true_center;
                                let u = uv[i];

                                let rot = Rotation3::new(Vector3D::x() * std::f32::consts::PI);
                                let rotated_pt = rot.transform_vector(&new_pt) + true_center;

                                pts.push(rotated_pt);
                                uv.push(u);

                                uvs.push(Vector2::new(u.x, u.y));
                                verts.push(Vector3::new(rotated_pt.x, rotated_pt.y, rotated_pt.z));
                            }

                            for _ in 0..pts.len() {
                                normals.push(Vector3::new(1.0, 0.0, 0.0));
                            }

                            for (pt, u) in pts.iter().zip(uv.iter()) {
                                let new_pt = pt - true_center;

                                let rot = Rotation3::new(Vector3D::y() * std::f32::consts::PI);
                                let rotated_pt = rot.transform_vector(&new_pt) + true_center;

                                uvs.push(Vector2::new(u.x, u.y));
                                verts.push(Vector3::new(rotated_pt.x, rotated_pt.y, rotated_pt.z));
                                normals.push(Vector3::new(-1.0,0.0,0.0));
                            }

                        }
                        2 => { //front and back
                            let mut pts: Vec<Vector3D> = Vec::new();

                            let top_right = Vector3D::new(max.x , max.y , min.z );
                            let inner_top_right = Vector3D::new( //inner top right
                                max.x  - margin,
                                max.y  - margin,
                                min.z 
                            );

                            pts.push(Vector3D::new(min.x , max.y , min.z )); //0 top left
                            pts.push(top_right); //1
                            pts.push(Vector3D::new( //2 inner top left
                                min.x  + margin,
                                max.y  - margin,
                                min.z 
                            ));
                            pts.push(inner_top_right); //3
                            pts.push(top_right); //4
                            pts.push(Vector3D::new(max.x , min.y , min.z )); //5 bottom right
                            pts.push(inner_top_right); //6
                            pts.push(Vector3D::new( //7 inner bottom right
                                max.x  - margin,
                                min.y  + margin,
                                min.z 
                            ));

                            let mut uv: Vec<Vector2D> = Vec::new();

                            uv.push(Vector2D::new(1.0 * abs_dimensions.x , 0.0));
                            uv.push(Vector2D::new(0.0, 0.0));
                            uv.push(Vector2D::new(1.0 * abs_dimensions.x  - margin, margin));
                            uv.push(Vector2D::new(margin, margin));

                            uv.push(Vector2D::new(1.0 * abs_dimensions.y , 0.0));
                            uv.push(Vector2D::new(0.0, 0.0));
                            uv.push(Vector2D::new(1.0 * abs_dimensions.y  - margin, margin));
                            uv.push(Vector2D::new(margin, margin));

                            for (pt, u) in pts.iter().zip(uv.iter()) {

                                uvs.push(Vector2::new(u.x, u.y));
                                verts.push(Vector3::new(pt.x, pt.y, pt.z));
                            }

                            let pts_len = pts.len();
                            for i in 0..pts_len {

                                let new_pt = pts[i] - true_center;
                                let u = uv[i];

                                let rot = Rotation3::new(Vector3D::z() * std::f32::consts::PI);
                                let rotated_pt = rot.transform_vector(&new_pt) + true_center;

                                pts.push(rotated_pt);
                                uv.push(u);

                                uvs.push(Vector2::new(u.x, u.y));
                                verts.push(Vector3::new(rotated_pt.x, rotated_pt.y, rotated_pt.z));
                            }

                            for _ in 0..pts.len() {
                                normals.push(Vector3::new(0.0, 0.0, 1.0));
                            }

                            for (pt, u) in pts.iter().zip(uv.iter()) {
                                let new_pt = pt - true_center;

                                let rot = Rotation3::new(Vector3D::y() * std::f32::consts::PI);
                                let rotated_pt = rot.transform_vector(&new_pt) + true_center;

                                uvs.push(Vector2::new(u.x, u.y));
                                verts.push(Vector3::new(rotated_pt.x, rotated_pt.y, rotated_pt.z));
                                normals.push(Vector3::new(0.0,0.0,-1.0));
                            }
                        },
                        _ => {}
                    } 

                    let mut indices: Vec<i32> = Vec::with_capacity(48);

                    //add indices for all "quads" in the face;
                    for j in 0..8 {
                        let k = offset + j*4;

                        indices.push(k+2);
                        indices.push(k+1);
                        indices.push(k);

                        indices.push(k+2);
                        indices.push(k+3);
                        indices.push(k+1);

                    }

                    //increase the offset for the next loop by the number of verts in the face before consuming verts
                    offset += verts.len() as i32;

                    mesh_data.verts.extend(verts);
                    mesh_data.normals.extend(normals);
                    mesh_data.uvs.extend(uvs);
                    mesh_data.indices.extend(indices);
 
                }

                // godot_print!("Updated selection box mesh");
                
            })

        })
}

fn expansion_movement_helper(expansion: Point, camera_adjusted_dir: CameraAdjustedDirection, new_aabb: &mut AABB) -> Point {

    let original = *new_aabb;

    new_aabb.dimensions += expansion;
    
    if new_aabb.dimensions.x == 0 {
        new_aabb.dimensions.x += expansion.x * 2;
    }

    if new_aabb.dimensions.y == 0 {
        new_aabb.dimensions.y += expansion.y * 2;
    }

    if new_aabb.dimensions.z == 0 {
        new_aabb.dimensions.z += expansion.z * 2;
    }

    let mut min = original.get_min();
    let mut max = original.get_max();

    let mut new_min = new_aabb.get_min();
    let mut new_max = new_aabb.get_max();

    // Adjust the offset based off of camera direction
    if camera_adjusted_dir.right.x < 0. { 
        let tmp_min = min.x;
        let tmp_new_min = new_min.x;
        min.x = max.x; 
        new_min.x = new_max.x; 
        max.x = tmp_min;
        new_max.x = tmp_new_min;
    } 
    if camera_adjusted_dir.right.z < 0. { 
        let tmp_min = min.z;
        let tmp_new_min = new_min.z;
        min.z = max.z; 
        new_min.z = new_max.z; 
        max.z = tmp_min;
        new_max.z = tmp_new_min;
    }

    Point::new(
        if new_aabb.dimensions.x < 0 { new_max.x - max.x } else { new_min.x - min.x },
        if new_aabb.dimensions.y < 0 { new_max.y - max.y } else { new_min.y - min.y },
        if new_aabb.dimensions.z < 0 { new_max.z - max.z } else { new_min.z - min.z },
    )
} 

pub fn actor_tool_rotation(world: &mut World, selection_entity: Entity, tool_rotation: Rotation3<f32>) {

    if let Some(Some((actor_entity, rotation, aabb))) = world.entry(selection_entity).map(|mut entry| {
        entry.get_component_mut::<SelectionBoxRotation>().map(|selection_box_rot| {
            selection_box_rot.value *= tool_rotation;
            selection_box_rot.value
        }).ok().and_then(|rotation| {
            entry.get_component_mut::<SelectionBox>().map(|selection_box| {
                selection_box.aabb = selection_box.aabb.rotate(tool_rotation);
                selection_box.aabb
            }).ok().and_then(|aabb| {
                entry.get_component_mut::<EntityRef>().map(|entity_ref| entity_ref.0)
                    .ok().map(|entity| (entity, rotation, aabb))
            })
        })
    }) {
        if let Some(mut entry) = world.entry(actor_entity) { 
            entry.add_component(transform::rotation::Rotation{
                value: rotation
            }); 
        }
        actor::position_actor_helper(world, actor_entity, aabb);
    }
}

/// Updates the selection box with the new chosen actor (new_entity should be newly duplicated into this world)
pub fn update_chosen_actor(world: &mut World, selection_entity: Entity, actor_id: i64) {

    // Check to see if there is an EntityRef which points to our old entity, and remove it
    if let Some(Some(old_entity)) = world.entry(selection_entity).map(|entry| {
        entry.get_component::<EntityRef>().map(|entity_ref| entity_ref.0).ok()
    }) {
        if let Some(Some(node)) = world.entry(old_entity).map(|actor_entry| {
            actor_entry.get_component::<node::NodeRef>().map(|node_ref| node_ref.val()).ok()
        }) {
            node::free(world, node)
        }
    }

    if let Some(actor_world) = ActorPalette::get_world() {
        let actor_world = &mut actor_world.borrow_mut();
        if let Some(actor_world) = actor_world.as_mut() {
            ENTITY_REFS.with(|e| {
                let entity_refs = e.borrow();

                if let Some(actor_entity) = entity_refs.get(&actor_id) {

                    MERGER.with(|m| {
                        let mut merger = m.borrow_mut();
                        let new_entity = world.clone_from_single(actor_world, *actor_entity, &mut *merger);

                        if let Some(bounds) = world.entry(new_entity).map(|entry| {
                            entry.get_component::<actor::Bounds>().map(|b| *b).ok()
                        }).flatten() { 
                            if let Some(Some((node_ref, rotation, aabb))) = world.entry(selection_entity).map(|mut entry| {
                                entry.add_component(EntityRef(new_entity));
                                entry.get_component::<SelectionBoxRotation>()
                                    .map(|box_rotation| box_rotation.value)
                                    .ok().and_then(|rotation| {
                                        entry.get_component_mut::<SelectionBox>().map(|selection_box| {
                                            selection_box.aabb = bounds.get_scaled_and_rotated_aabb(rotation);
                                            selection_box.aabb
                                        }).ok().and_then(|aabb| {
                                            entry.get_component::<node::NodeRef>().map(|n| n.val()).ok()
                                                .map(|node_ref| (node_ref, rotation, aabb))
                                        })
                                    })
                            }) {

                                if let Some(mut entry) = world.entry(new_entity) {
                                    entry.add_component(transform::rotation::Rotation{
                                        value: rotation
                                    });

                                    entry.add_component(node::NodeParent::new(node_ref));

                                }
                                actor::position_actor_helper(world, new_entity, aabb);
                            }
                        }
                    });
                }
            });
        }
    }
}

pub fn get_box_entity_by_client_id<T: legion::storage::Component>(world: &mut World, client_id: ClientID) -> Option<Entity> {

    let mut query = <(Entity, Read<ClientID>)>::query().filter(component::<SelectionBox>() & component::<T>());

    query.iter(world)
        .filter(|(_, id)| id.val() == client_id.val())
        .map(|(entity, _)| *entity)
        .next()
    
}

pub fn set_active_selection_box<T: legion::storage::Component>(world: &mut World, client_id: ClientID) {

    //disable active selection box that is not this component type
    let mut query = <(Entity, Read<ClientID>, Read<node::NodeRef>)>::query().filter(component::<SelectionBox>() & component::<Active>() & !component::<T>());
    let results = query.iter(world)
        .filter(|(_, id, _)| client_id == **id)
        .map(|(entity, _, node_ref)| (*entity, node_ref.val()))
        .collect::<Vec<(Entity, Ref<Node>)>>();

    for (entity, node_ref) in results {

        let mesh = unsafe { node_ref.assume_safe().cast::<Spatial>().unwrap()};

        mesh.set_visible(false);

        if let Some(mut entry) = world.entry(entity) {
            entry.remove_component::<Active>();
        }
    }

    //enable selection box that is not yet active and that is this component type
    let mut query = <(Entity, Read<ClientID>, Read<node::NodeRef>)>::query().filter(component::<SelectionBox>() & !component::<Active>() & component::<T>());
    let results = query.iter(world)
        .filter(|(_, id, _)| {
            client_id == **id
        })
        .map(|(entity, _, node_ref)| (*entity, node_ref.val()))
        .collect::<Vec<(Entity, Ref<Node>)>>();

    for (entity, node_ref) in results {

        let mesh = unsafe { node_ref.assume_safe().cast::<Spatial>().unwrap()};

        mesh.set_visible(true);

        if let Some(mut entry) = world.entry(entity) {
            entry.add_component(Active{});
        }
    }

}
    