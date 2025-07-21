use fast_surface_nets::glam::{Vec2, Vec3A};
use fast_surface_nets::ndshape::{ConstShape, ConstShape3u32};
use fast_surface_nets::{
    surface_nets, surface_nets_with_config, SurfaceNetsBuffer, SurfaceNetsConfig,
};

use bevy::{
    prelude::*,
    render::{
        mesh::{Indices, VertexAttributeValues},
        render_resource::PrimitiveTopology,
    },
};
use obj_exporter::{export_to_file, Geometry, ObjSet, Object, Primitive, Shape, Vertex};

fn main() {
    App::new()
        //.insert_resource(Msaa::Sample4)
        .add_plugins(DefaultPlugins)
        .add_systems(Startup, setup)
        .run();
}

fn setup(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {

    commands.spawn((
        PointLight {
            shadows_enabled: true,
            range: 200.0,
            intensity: 800000.0, // Adjusted for Bevy 0.16's lighting changes
            ..default()
        },
        Transform::from_translation(Vec3::new(25.0, 25.0, 25.0)),
    ));

    commands.spawn((
        Camera3d::default(),
        Transform::from_translation(Vec3::new(70.0, 15.0, 70.0))
            .looking_at(Vec3::new(0.0, 0.0, 0.0), Vec3::Y),
    ));

    // Generate regular (open) meshes
    let (sphere_buffer, sphere_mesh) = sdf_to_mesh(&mut meshes, |p| sphere(1.3, p), true);
    let (cube_buffer, cube_mesh) = sdf_to_mesh(&mut meshes, |p| cube(Vec3A::splat(0.5), p), true);
    let (link_buffer, link_mesh) = sdf_to_mesh(&mut meshes, |p| link(0.26, 0.4, 0.18, p), true);

    // Generate watertight meshes
    let (sphere_watertight_buffer, sphere_watertight_mesh) =
        sdf_to_mesh(&mut meshes, |p| sphere(1.3, p), true);
    let (cube_watertight_buffer, cube_watertight_mesh) =
        sdf_to_mesh(&mut meshes, |p| cube(Vec3A::splat(0.5), p), true);
    let (link_watertight_buffer, link_watertight_mesh) =
        sdf_to_mesh(&mut meshes, |p| link(0.26, 0.4, 0.18, p), true);

    // Spawn regular meshes on the left
    spawn_pbr(
        &mut commands,
        &mut materials,
        sphere_mesh,
        Transform::from_translation(Vec3::new(-16.0, -16.0, -16.0)),
    );
    spawn_pbr(
        &mut commands,
        &mut materials,
        cube_mesh,
        Transform::from_translation(Vec3::new(-16.0, -16.0, 16.0)),
    );
    spawn_pbr(
        &mut commands,
        &mut materials,
        link_mesh,
        Transform::from_translation(Vec3::new(-16.0, 16.0, -16.0)),
    );

    // Spawn watertight meshes on the right
    spawn_pbr(
        &mut commands,
        &mut materials,
        sphere_watertight_mesh,
        Transform::from_translation(Vec3::new(16.0, -16.0, -16.0)),
    );
    spawn_pbr(
        &mut commands,
        &mut materials,
        cube_watertight_mesh,
        Transform::from_translation(Vec3::new(16.0, -16.0, 16.0)),
    );
    spawn_pbr(
        &mut commands,
        &mut materials,
        link_watertight_mesh,
        Transform::from_translation(Vec3::new(16.0, 16.0, -16.0)),
    );

    write_mesh_to_obj_file("sphere".into(), &sphere_buffer);
    write_mesh_to_obj_file("cube".into(), &cube_buffer);
    write_mesh_to_obj_file("link".into(), &link_buffer);
    write_mesh_to_obj_file("sphere_watertight".into(), &sphere_watertight_buffer);
    write_mesh_to_obj_file("cube_watertight".into(), &cube_watertight_buffer);
    write_mesh_to_obj_file("link_watertight".into(), &link_watertight_buffer);
}

fn sdf_to_mesh(
    meshes: &mut Assets<Mesh>,
    sdf: impl Fn(Vec3A) -> f32,
    watertight: bool,
) -> (SurfaceNetsBuffer, Handle<Mesh>) {
    type SampleShape = ConstShape3u32<34, 34, 34>;

    let mut samples = [1.0; SampleShape::SIZE as usize];
    for i in 0u32..(SampleShape::SIZE) {
        let p = into_domain(32, SampleShape::delinearize(i));
        samples[i as usize] = sdf(p);
    }

    let mut buffer = SurfaceNetsBuffer::default();

    if watertight {
        let config = SurfaceNetsConfig {
            generate_boundary_faces: true,
        };
        surface_nets_with_config(
            &samples,
            &SampleShape {},
            [0; 3],
            [33; 3],
            config,
            &mut buffer,
        );
    } else {
        surface_nets(&samples, &SampleShape {}, [0; 3], [33; 3], &mut buffer);
    }

    let num_vertices = buffer.positions.len();

    let mut render_mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        bevy::render::render_asset::RenderAssetUsages::RENDER_WORLD,
    );
    render_mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        VertexAttributeValues::Float32x3(buffer.positions.clone()),
    );
    render_mesh.insert_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        VertexAttributeValues::Float32x3(buffer.normals.clone()),
    );
    render_mesh.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        VertexAttributeValues::Float32x2(vec![[0.0; 2]; num_vertices]),
    );
    render_mesh.insert_indices(Indices::U32(buffer.indices.clone()));

    (buffer, meshes.add(render_mesh))
}

fn spawn_pbr(
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    mesh: Handle<Mesh>,
    transform: Transform,
) {
    let mut material = StandardMaterial::from(Color::srgb(1.0, 1.0, 1.0));
    material.perceptual_roughness = 0.9;

    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(materials.add(material)),
        transform,
    ));
}

fn write_mesh_to_obj_file(name: String, buffer: &SurfaceNetsBuffer) {
    let filename = format!("{}.obj", name);
    export_to_file(
        &ObjSet {
            material_library: None,
            objects: vec![Object {
                name,
                vertices: buffer
                    .positions
                    .iter()
                    .map(|&[x, y, z]| Vertex {
                        x: x as f64,
                        y: y as f64,
                        z: z as f64,
                    })
                    .collect(),
                normals: buffer
                    .normals
                    .iter()
                    .map(|&[x, y, z]| Vertex {
                        x: x as f64,
                        y: y as f64,
                        z: z as f64,
                    })
                    .collect(),
                geometry: vec![Geometry {
                    material_name: None,
                    shapes: buffer
                        .indices
                        .chunks(3)
                        .map(|tri| Shape {
                            primitive: Primitive::Triangle(
                                (tri[0] as usize, None, Some(tri[0] as usize)),
                                (tri[1] as usize, None, Some(tri[1] as usize)),
                                (tri[2] as usize, None, Some(tri[2] as usize)),
                            ),
                            groups: vec![],
                            smoothing_groups: vec![],
                        })
                        .collect(),
                }],
                tex_vertices: vec![],
            }],
        },
        filename,
    )
    .unwrap();
}

fn into_domain(array_dim: u32, [x, y, z]: [u32; 3]) -> Vec3A {
    (2.0 / array_dim as f32) * Vec3A::new(x as f32, y as f32, z as f32) - 1.0
}

fn sphere(radius: f32, p: Vec3A) -> f32 {
    p.length() - radius
}

fn cube(b: Vec3A, p: Vec3A) -> f32 {
    let q = p.abs() - b;
    q.max(Vec3A::ZERO).length() + q.max_element().min(0.0)
}

fn link(le: f32, r1: f32, r2: f32, p: Vec3A) -> f32 {
    let q = Vec3A::new(p.x, (p.y.abs() - le).max(0.0), p.z);
    Vec2::new(q.length() - r1, q.z).length() - r2
}
