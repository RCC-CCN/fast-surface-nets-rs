//! A fast, chunk-friendly implementation of Naive Surface Nets on regular grids.
//!
//! ![Mesh
//! Examples](https://raw.githubusercontent.com/bonsairobo/fast-surface-nets-rs/main/examples-crate/render/mesh_examples.png)
//!
//! Surface Nets is an algorithm for extracting an isosurface mesh from a [signed distance
//! field](https://en.wikipedia.org/wiki/Signed_distance_function) sampled on a regular grid. It is nearly the same as Dual
//! Contouring, but instead of using hermite (derivative) data to estimate surface points, Surface Nets will do a simpler form
//! of interpolation (average) between points where the isosurface crosses voxel cube edges.
//!
//! Benchmarks show that [`surface_nets`] generates about 20 million triangles per second on a single core
//! of a 2.5 GHz Intel Core i7. This implementation achieves high performance by using small lookup tables and SIMD acceleration
//! provided by `glam` when doing 3D floating point vector math. (Users are not required to use `glam` types in any API
//! signatures.) To run the benchmarks yourself, `cd bench/ && cargo bench`.
//!
//! High-quality surface normals are estimated by:
//!
//! 1. calculating SDF derivatives using central differencing
//! 2. using bilinear interpolation of SDF derivatives along voxel cube edges
//!
//! When working with sparse data sets, [`surface_nets`] can generate meshes for array chunks that fit
//! together seamlessly. This works because faces are not generated on the positive boundaries of a chunk. One must only apply a
//! translation of the mesh into proper world coordinates for the given chunk.
//!
//! # Example Code
//!
//! ```
//! use fast_surface_nets::ndshape::{ConstShape, ConstShape3u32};
//! use fast_surface_nets::{surface_nets, surface_nets_with_config, SurfaceNetsBuffer, SurfaceNetsConfig};
//!
//! // A 16^3 chunk with 1-voxel boundary padding.
//! type ChunkShape = ConstShape3u32<18, 18, 18>;
//!
//! // This chunk will cover just a single octant of a sphere SDF (radius 15).
//! let mut sdf = [1.0; ChunkShape::USIZE];
//! for i in 0u32..ChunkShape::SIZE {
//!     let [x, y, z] = ChunkShape::delinearize(i);
//!     sdf[i as usize] = ((x * x + y * y + z * z) as f32).sqrt() - 15.0;
//! }
//!
//! let mut buffer = SurfaceNetsBuffer::default();
//! surface_nets(&sdf, &ChunkShape {}, [0; 3], [17; 3], &mut buffer);
//!
//! // Some triangles were generated.
//! assert!(!buffer.indices.is_empty());
//!
//! // For watertight meshes, use surface_nets_with_config:
//! let mut watertight_buffer = SurfaceNetsBuffer::default();
//! let config = SurfaceNetsConfig {
//!     generate_boundary_faces: true,
//! };
//! surface_nets_with_config(&sdf, &ChunkShape {}, [0; 3], [17; 3], config, &mut watertight_buffer);
//!
//! // The watertight mesh will have more triangles due to boundary faces.
//! assert!(watertight_buffer.indices.len() >= buffer.indices.len());
//! ```

pub use glam;
pub use ndshape;

use glam::{Vec3A, Vec3Swizzles};
use ndshape::Shape;

/// Configuration options for surface mesh generation.
#[derive(Debug, Clone, Copy)]
pub struct SurfaceNetsConfig {
    /// Whether to generate faces on the boundaries of the sampling volume to create watertight meshes.
    /// When enabled, faces will be generated on cube boundaries where the SDF is negative.
    pub generate_boundary_faces: bool,
}

impl Default for SurfaceNetsConfig {
    fn default() -> Self {
        Self {
            generate_boundary_faces: false,
        }
    }
}

pub trait SignedDistance: Into<f32> + Copy {
    fn is_negative(self) -> bool;
}

impl SignedDistance for f32 {
    fn is_negative(self) -> bool {
        self < 0.0
    }
}

/// The output buffers used by [`surface_nets`]. These buffers can be reused to avoid reallocating memory.
#[derive(Default, Clone)]
pub struct SurfaceNetsBuffer {
    /// The triangle mesh positions.
    ///
    /// These are in array-local coordinates, i.e. at array position `(x, y, z)`, the vertex position would be `(x, y, z) +
    /// centroid` if the isosurface intersects that voxel.
    pub positions: Vec<[f32; 3]>,
    /// The triangle mesh normals.
    ///
    /// The normals are **not** normalized, since that is done most efficiently on the GPU.
    pub normals: Vec<[f32; 3]>,
    /// The triangle mesh indices.
    pub indices: Vec<u32>,

    /// Local 3D array coordinates of every voxel that intersects the isosurface.
    pub surface_points: Vec<[u32; 3]>,
    /// Stride of every voxel that intersects the isosurface. Can be used for efficient post-processing.
    pub surface_strides: Vec<u32>,
    /// Used to map back from voxel stride to vertex index.
    pub stride_to_index: Vec<u32>,
}

impl SurfaceNetsBuffer {
    /// Clears all of the buffers, but keeps the memory allocated for reuse.
    fn reset(&mut self, array_size: usize) {
        self.positions.clear();
        self.normals.clear();
        self.indices.clear();
        self.surface_points.clear();
        self.surface_strides.clear();

        // Just make sure this buffer is big enough, whether or not we've used it before.
        self.stride_to_index.resize(array_size, NULL_VERTEX);
    }
}

/// This stride of the SDF array did not produce a vertex.
pub const NULL_VERTEX: u32 = u32::MAX;

/// The Naive Surface Nets smooth voxel meshing algorithm.
///
/// Extracts an isosurface mesh from the [signed distance field](https://en.wikipedia.org/wiki/Signed_distance_function) `sdf`.
/// Each value in the field determines how close that point is to the isosurface. Negative values are considered "interior" of
/// the surface volume, and positive values are considered "exterior." These lattice points will be considered corners of unit
/// cubes. For each unit cube, at most one isosurface vertex will be estimated, as below, where `p` is a positive corner value,
/// `n` is a negative corner value, `s` is an isosurface vertex, and `|` or `-` are mesh polygons connecting the vertices.
///
/// ```text
/// p   p   p   p
///   s---s
/// p | n | p   p
///   s   s---s
/// p | n   n | p
///   s---s---s
/// p   p   p   p
/// ```
///
/// The set of corners sampled is exactly the set of points in `[min, max]`. `sdf` must contain all of those points.
///
/// Note that the scheme illustrated above implies that chunks must be padded with a 1-voxel border copied from neighboring
/// voxels in order to connect seamlessly.
pub fn surface_nets<T, S>(
    sdf: &[T],
    shape: &S,
    min: [u32; 3],
    max: [u32; 3],
    output: &mut SurfaceNetsBuffer,
) where
    T: SignedDistance,
    S: Shape<3, Coord = u32>,
{
    surface_nets_with_config(sdf, shape, min, max, SurfaceNetsConfig::default(), output);
}

/// The Naive Surface Nets smooth voxel meshing algorithm with configuration options.
///
/// Extracts an isosurface mesh from the [signed distance field](https://en.wikipedia.org/wiki/Signed_distance_function) `sdf`
/// with additional configuration options for controlling mesh generation behavior.
///
/// When `config.generate_boundary_faces` is true, this function will generate faces on the boundaries of the sampling volume
/// where the SDF is negative, creating watertight meshes.
pub fn surface_nets_with_config<T, S>(
    sdf: &[T],
    shape: &S,
    min: [u32; 3],
    max: [u32; 3],
    config: SurfaceNetsConfig,
    output: &mut SurfaceNetsBuffer,
) where
    T: SignedDistance,
    S: Shape<3, Coord = u32>,
{
    // SAFETY
    // Make sure the slice matches the shape before we start using get_unchecked.
    assert!(shape.linearize(min) <= shape.linearize(max));
    assert!((shape.linearize(max) as usize) < sdf.len());

    output.reset(sdf.len());

    estimate_surface(sdf, shape, min, max, output);
    make_all_quads(sdf, shape, min, max, output);
    
    if config.generate_boundary_faces {
        make_boundary_faces(sdf, shape, min, max, output);
    }
}

// Find all vertex positions and normals. Also generate a map from grid position to vertex index to be used to look up vertices
// when generating quads.
fn estimate_surface<T, S>(
    sdf: &[T],
    shape: &S,
    [minx, miny, minz]: [u32; 3],
    [maxx, maxy, maxz]: [u32; 3],
    output: &mut SurfaceNetsBuffer,
) where
    T: SignedDistance,
    S: Shape<3, Coord = u32>,
{
    for z in minz..maxz {
        for y in miny..maxy {
            for x in minx..maxx {
                let stride = shape.linearize([x, y, z]);
                let p = Vec3A::from([x as f32, y as f32, z as f32]);
                if estimate_surface_in_cube(sdf, shape, p, stride, output) {
                    output.stride_to_index[stride as usize] = output.positions.len() as u32 - 1;
                    output.surface_points.push([x, y, z]);
                    output.surface_strides.push(stride);
                } else {
                    output.stride_to_index[stride as usize] = NULL_VERTEX;
                }
            }
        }
    }
}

// Consider the grid-aligned cube where `p` is the minimal corner. Find a point inside this cube that is approximately on the
// isosurface.
//
// This is done by estimating, for each cube edge, where the isosurface crosses the edge (if it does at all). Then the estimated
// surface point is the average of these edge crossings.
fn estimate_surface_in_cube<T, S>(
    sdf: &[T],
    shape: &S,
    p: Vec3A,
    min_corner_stride: u32,
    output: &mut SurfaceNetsBuffer,
) -> bool
where
    T: SignedDistance,
    S: Shape<3, Coord = u32>,
{
    // Get the signed distance values at each corner of this cube.
    let mut corner_dists = [0f32; 8];
    let mut num_negative = 0;
    for (i, dist) in corner_dists.iter_mut().enumerate() {
        let corner_stride = min_corner_stride + shape.linearize(CUBE_CORNERS[i]);
        let d = *unsafe { sdf.get_unchecked(corner_stride as usize) };
        *dist = d.into();
        if d.is_negative() {
            num_negative += 1;
        }
    }

    if num_negative == 0 || num_negative == 8 {
        // No crossings.
        return false;
    }

    let c = centroid_of_edge_intersections(&corner_dists);

    output.positions.push((p + c).into());
    output.normals.push(sdf_gradient(&corner_dists, c).into());

    true
}

fn centroid_of_edge_intersections(dists: &[f32; 8]) -> Vec3A {
    let mut count = 0;
    let mut sum = Vec3A::ZERO;
    for &[corner1, corner2] in CUBE_EDGES.iter() {
        let d1 = dists[corner1 as usize];
        let d2 = dists[corner2 as usize];
        if (d1 < 0.0) != (d2 < 0.0) {
            count += 1;
            sum += estimate_surface_edge_intersection(corner1, corner2, d1, d2);
        }
    }

    sum / count as f32
}

// Given two cube corners, find the point between them where the SDF is zero. (This might not exist).
fn estimate_surface_edge_intersection(
    corner1: u32,
    corner2: u32,
    value1: f32,
    value2: f32,
) -> Vec3A {
    let interp1 = value1 / (value1 - value2);
    let interp2 = 1.0 - interp1;

    interp2 * CUBE_CORNER_VECTORS[corner1 as usize]
        + interp1 * CUBE_CORNER_VECTORS[corner2 as usize]
}

/// Calculate the normal as the gradient of the distance field. Don't bother making it a unit vector, since we'll do that on the
/// GPU.
///
/// For each dimension, there are 4 cube edges along that axis. This will do bilinear interpolation between the differences
/// along those edges based on the position of the surface (s).
fn sdf_gradient(dists: &[f32; 8], s: Vec3A) -> Vec3A {
    let p00 = Vec3A::from([dists[0b001], dists[0b010], dists[0b100]]);
    let n00 = Vec3A::from([dists[0b000], dists[0b000], dists[0b000]]);

    let p10 = Vec3A::from([dists[0b101], dists[0b011], dists[0b110]]);
    let n10 = Vec3A::from([dists[0b100], dists[0b001], dists[0b010]]);

    let p01 = Vec3A::from([dists[0b011], dists[0b110], dists[0b101]]);
    let n01 = Vec3A::from([dists[0b010], dists[0b100], dists[0b001]]);

    let p11 = Vec3A::from([dists[0b111], dists[0b111], dists[0b111]]);
    let n11 = Vec3A::from([dists[0b110], dists[0b101], dists[0b011]]);

    // Each dimension encodes an edge delta, giving 12 in total.
    let d00 = p00 - n00; // Edges (0b00x, 0b0y0, 0bz00)
    let d10 = p10 - n10; // Edges (0b10x, 0b0y1, 0bz10)
    let d01 = p01 - n01; // Edges (0b01x, 0b1y0, 0bz01)
    let d11 = p11 - n11; // Edges (0b11x, 0b1y1, 0bz11)

    let neg = Vec3A::ONE - s;

    // Do bilinear interpolation between 4 edges in each dimension.
    neg.yzx() * neg.zxy() * d00
        + neg.yzx() * s.zxy() * d10
        + s.yzx() * neg.zxy() * d01
        + s.yzx() * s.zxy() * d11
}

// For every edge that crosses the isosurface, make a quad between the "centers" of the four cubes touching that surface. The
// "centers" are actually the vertex positions found earlier. Also make sure the triangles are facing the right way. See the
// comments on `maybe_make_quad` to help with understanding the indexing.
fn make_all_quads<T, S>(
    sdf: &[T],
    shape: &S,
    [minx, miny, minz]: [u32; 3],
    [maxx, maxy, maxz]: [u32; 3],
    output: &mut SurfaceNetsBuffer,
) where
    T: SignedDistance,
    S: Shape<3, Coord = u32>,
{
    let xyz_strides = [
        shape.linearize([1, 0, 0]) as usize,
        shape.linearize([0, 1, 0]) as usize,
        shape.linearize([0, 0, 1]) as usize,
    ];

    for (&[x, y, z], &p_stride) in output
        .surface_points
        .iter()
        .zip(output.surface_strides.iter())
    {
        let p_stride = p_stride as usize;
        let eval_max_plane = cfg!(feature = "eval-max-plane");

        // Do edges parallel with the X axis
        if y != miny && z != minz && (eval_max_plane || x != maxx - 1) {
            maybe_make_quad(
                sdf,
                &output.stride_to_index,
                &output.positions,
                p_stride,
                p_stride + xyz_strides[0],
                xyz_strides[1],
                xyz_strides[2],
                &mut output.indices,
            );
        }
        // Do edges parallel with the Y axis
        if x != minx && z != minz && (eval_max_plane || y != maxy - 1) {
            maybe_make_quad(
                sdf,
                &output.stride_to_index,
                &output.positions,
                p_stride,
                p_stride + xyz_strides[1],
                xyz_strides[2],
                xyz_strides[0],
                &mut output.indices,
            );
        }
        // Do edges parallel with the Z axis
        if x != minx && y != miny && (eval_max_plane || z != maxz - 1) {
            maybe_make_quad(
                sdf,
                &output.stride_to_index,
                &output.positions,
                p_stride,
                p_stride + xyz_strides[2],
                xyz_strides[0],
                xyz_strides[1],
                &mut output.indices,
            );
        }
    }
}

// Construct a quad in the dual graph of the SDF lattice.
//
// The surface point s was found somewhere inside of the cube with minimal corner p1.
//
//       x ---- x
//      /      /|
//     x ---- x |
//     |   s  | x
//     |      |/
//    p1 --- p2
//
// And now we want to find the quad between p1 and p2 where s is a corner of the quad.
//
//          s
//         /|
//        / |
//       |  |
//   p1  |  |  p2
//       | /
//       |/
//
// If A is (of the three grid axes) the axis between p1 and p2,
//
//       A
//   p1 ---> p2
//
// then we must find the other 3 quad corners by moving along the other two axes (those orthogonal to A) in the negative
// directions; these are axis B and axis C.
#[allow(clippy::too_many_arguments)]
fn maybe_make_quad<T>(
    sdf: &[T],
    stride_to_index: &[u32],
    positions: &[[f32; 3]],
    p1: usize,
    p2: usize,
    axis_b_stride: usize,
    axis_c_stride: usize,
    indices: &mut Vec<u32>,
) where
    T: SignedDistance,
{
    let d1 = unsafe { sdf.get_unchecked(p1) };
    let d2 = unsafe { sdf.get_unchecked(p2) };
    let negative_face = match (d1.is_negative(), d2.is_negative()) {
        (true, false) => false,
        (false, true) => true,
        _ => return, // No face.
    };

    // The triangle points, viewed face-front, look like this:
    // v1 v3
    // v2 v4
    let v1 = stride_to_index[p1];
    let v2 = stride_to_index[p1 - axis_b_stride];
    let v3 = stride_to_index[p1 - axis_c_stride];
    let v4 = stride_to_index[p1 - axis_b_stride - axis_c_stride];
    let (pos1, pos2, pos3, pos4) = (
        Vec3A::from(positions[v1 as usize]),
        Vec3A::from(positions[v2 as usize]),
        Vec3A::from(positions[v3 as usize]),
        Vec3A::from(positions[v4 as usize]),
    );
    // Split the quad along the shorter axis, rather than the longer one.
    let quad = if pos1.distance_squared(pos4) < pos2.distance_squared(pos3) {
        if negative_face {
            [v1, v4, v2, v1, v3, v4]
        } else {
            [v1, v2, v4, v1, v4, v3]
        }
    } else if negative_face {
        [v2, v3, v4, v2, v1, v3]
    } else {
        [v2, v4, v3, v2, v3, v1]
    };
    indices.extend_from_slice(&quad);
}

// Generate faces on the boundaries of the sampling volume where the SDF is negative.
// This creates watertight meshes by closing holes at the boundaries.
fn make_boundary_faces<T, S>(
    sdf: &[T],
    shape: &S,
    [minx, miny, minz]: [u32; 3],
    [maxx, maxy, maxz]: [u32; 3],
    output: &mut SurfaceNetsBuffer,
) where
    T: SignedDistance,
    S: Shape<3, Coord = u32>,
{
    // First, generate boundary vertices where needed
    generate_boundary_vertices(sdf, shape, [minx, miny, minz], [maxx, maxy, maxz], output);
    
    // Then generate boundary faces
    make_boundary_faces_x(sdf, shape, [minx, miny, minz], [maxx, maxy, maxz], minx, output);
    make_boundary_faces_x(sdf, shape, [minx, miny, minz], [maxx, maxy, maxz], maxx - 1, output);
    make_boundary_faces_y(sdf, shape, [minx, miny, minz], [maxx, maxy, maxz], miny, output);
    make_boundary_faces_y(sdf, shape, [minx, miny, minz], [maxx, maxy, maxz], maxy - 1, output);
    make_boundary_faces_z(sdf, shape, [minx, miny, minz], [maxx, maxy, maxz], minz, output);
    make_boundary_faces_z(sdf, shape, [minx, miny, minz], [maxx, maxy, maxz], maxz - 1, output);
}

// Generate boundary vertices for negative SDF values at the boundaries
fn generate_boundary_vertices<T, S>(
    sdf: &[T],
    shape: &S,
    [minx, miny, minz]: [u32; 3],
    [maxx, maxy, maxz]: [u32; 3],
    output: &mut SurfaceNetsBuffer,
) where
    T: SignedDistance,
    S: Shape<3, Coord = u32>,
{
    // Use a map to track vertex positions and avoid duplicates
    use std::collections::HashMap;
    let mut position_to_index: HashMap<[u32; 3], u32> = HashMap::new();
    
    // First, map existing vertices to their positions
    for (stride_idx, &vertex_idx) in output.stride_to_index.iter().enumerate() {
        if vertex_idx != NULL_VERTEX {
            // Find the 3D position from the stride
            let coords = shape.delinearize(stride_idx as u32);
            position_to_index.insert(coords, vertex_idx);
        }
    }
    
    // Check boundary voxels and create vertices for negative SDF values
    for z in minz..maxz {
        for y in miny..maxy {
            for x in minx..maxx {
                let is_boundary = x == minx || x == maxx - 1 || y == miny || y == maxy - 1 || z == minz || z == maxz - 1;
                
                if is_boundary {
                    let stride = shape.linearize([x, y, z]);
                    
                    // Only create boundary vertex if not already created
                    if output.stride_to_index[stride as usize] == NULL_VERTEX {
                        let sdf_value = unsafe { sdf.get_unchecked(stride as usize) };
                        
                        if sdf_value.is_negative() {
                            // Calculate the target boundary position
                            let boundary_pos = if x == minx {
                                [minx as f32, y as f32 + 0.5, z as f32 + 0.5]
                            } else if x == maxx - 1 {
                                [(maxx - 1) as f32 + 1.0, y as f32 + 0.5, z as f32 + 0.5]
                            } else if y == miny {
                                [x as f32 + 0.5, miny as f32, z as f32 + 0.5]
                            } else if y == maxy - 1 {
                                [x as f32 + 0.5, (maxy - 1) as f32 + 1.0, z as f32 + 0.5]
                            } else if z == minz {
                                [x as f32 + 0.5, y as f32 + 0.5, minz as f32]
                            } else { // z == maxz - 1
                                [x as f32 + 0.5, y as f32 + 0.5, (maxz - 1) as f32 + 1.0]
                            };
                            
                            // Check if we already have a vertex at this exact position
                            let mut existing_vertex_idx = None;
                            for (i, &pos) in output.positions.iter().enumerate() {
                                if (pos[0] - boundary_pos[0]).abs() < 0.001 
                                    && (pos[1] - boundary_pos[1]).abs() < 0.001 
                                    && (pos[2] - boundary_pos[2]).abs() < 0.001 {
                                    existing_vertex_idx = Some(i as u32);
                                    break;
                                }
                            }
                            
                            let vertex_idx = if let Some(idx) = existing_vertex_idx {
                                // Reuse existing vertex
                                idx
                            } else {
                                // Create new vertex
                                let normal = if x == minx {
                                    [-1.0, 0.0, 0.0]
                                } else if x == maxx - 1 {
                                    [1.0, 0.0, 0.0]
                                } else if y == miny {
                                    [0.0, -1.0, 0.0]
                                } else if y == maxy - 1 {
                                    [0.0, 1.0, 0.0]
                                } else if z == minz {
                                    [0.0, 0.0, -1.0]
                                } else {
                                    [0.0, 0.0, 1.0]
                                };
                                
                                output.positions.push(boundary_pos);
                                output.normals.push(normal);
                                output.surface_points.push([x, y, z]);
                                output.surface_strides.push(stride);
                                (output.positions.len() - 1) as u32
                            };
                            
                            output.stride_to_index[stride as usize] = vertex_idx;
                        }
                    }
                }
            }
        }
    }
}

// Generate boundary faces for X planes
fn make_boundary_faces_x<T, S>(
    sdf: &[T],
    shape: &S,
    [minx, miny, minz]: [u32; 3],
    [maxx, maxy, maxz]: [u32; 3],
    x_plane: u32,
    output: &mut SurfaceNetsBuffer,
) where
    T: SignedDistance,
    S: Shape<3, Coord = u32>,
{
    let is_min_face = x_plane == minx;
    
    for z in minz..(maxz - 1) {
        for y in miny..(maxy - 1) {
            // Get the four corners of the quad
            let stride_00 = shape.linearize([x_plane, y, z]);
            let stride_01 = shape.linearize([x_plane, y, z + 1]);
            let stride_10 = shape.linearize([x_plane, y + 1, z]);
            let stride_11 = shape.linearize([x_plane, y + 1, z + 1]);
            
            let v00 = output.stride_to_index[stride_00 as usize];
            let v01 = output.stride_to_index[stride_01 as usize];
            let v10 = output.stride_to_index[stride_10 as usize];
            let v11 = output.stride_to_index[stride_11 as usize];
            
            // Only create faces if all vertices exist
            if v00 != NULL_VERTEX && v01 != NULL_VERTEX && v10 != NULL_VERTEX && v11 != NULL_VERTEX {
                if is_min_face {
                    // Winding for min face (facing outward)
                    output.indices.extend_from_slice(&[v00, v01, v10]);
                    output.indices.extend_from_slice(&[v01, v11, v10]);
                } else {
                    // Winding for max face (facing outward)
                    output.indices.extend_from_slice(&[v00, v10, v01]);
                    output.indices.extend_from_slice(&[v01, v10, v11]);
                }
            }
        }
    }
}

// Generate boundary faces for Y planes
fn make_boundary_faces_y<T, S>(
    sdf: &[T],
    shape: &S,
    [minx, miny, minz]: [u32; 3],
    [maxx, maxy, maxz]: [u32; 3],
    y_plane: u32,
    output: &mut SurfaceNetsBuffer,
) where
    T: SignedDistance,
    S: Shape<3, Coord = u32>,
{
    let is_min_face = y_plane == miny;
    
    for z in minz..(maxz - 1) {
        for x in minx..(maxx - 1) {
            let stride_00 = shape.linearize([x, y_plane, z]);
            let stride_01 = shape.linearize([x, y_plane, z + 1]);
            let stride_10 = shape.linearize([x + 1, y_plane, z]);
            let stride_11 = shape.linearize([x + 1, y_plane, z + 1]);
            
            let v00 = output.stride_to_index[stride_00 as usize];
            let v01 = output.stride_to_index[stride_01 as usize];
            let v10 = output.stride_to_index[stride_10 as usize];
            let v11 = output.stride_to_index[stride_11 as usize];
            
            if v00 != NULL_VERTEX && v01 != NULL_VERTEX && v10 != NULL_VERTEX && v11 != NULL_VERTEX {
                if is_min_face {
                    output.indices.extend_from_slice(&[v00, v10, v01]);
                    output.indices.extend_from_slice(&[v01, v10, v11]);
                } else {
                    output.indices.extend_from_slice(&[v00, v01, v10]);
                    output.indices.extend_from_slice(&[v01, v11, v10]);
                }
            }
        }
    }
}

// Generate boundary faces for Z planes
fn make_boundary_faces_z<T, S>(
    sdf: &[T],
    shape: &S,
    [minx, miny, minz]: [u32; 3],
    [maxx, maxy, maxz]: [u32; 3],
    z_plane: u32,
    output: &mut SurfaceNetsBuffer,
) where
    T: SignedDistance,
    S: Shape<3, Coord = u32>,
{
    let is_min_face = z_plane == minz;
    
    for y in miny..(maxy - 1) {
        for x in minx..(maxx - 1) {
            let stride_00 = shape.linearize([x, y, z_plane]);
            let stride_01 = shape.linearize([x, y + 1, z_plane]);
            let stride_10 = shape.linearize([x + 1, y, z_plane]);
            let stride_11 = shape.linearize([x + 1, y + 1, z_plane]);
            
            let v00 = output.stride_to_index[stride_00 as usize];
            let v01 = output.stride_to_index[stride_01 as usize];
            let v10 = output.stride_to_index[stride_10 as usize];
            let v11 = output.stride_to_index[stride_11 as usize];
            
            if v00 != NULL_VERTEX && v01 != NULL_VERTEX && v10 != NULL_VERTEX && v11 != NULL_VERTEX {
                if is_min_face {
                    output.indices.extend_from_slice(&[v00, v01, v10]);
                    output.indices.extend_from_slice(&[v01, v11, v10]);
                } else {
                    output.indices.extend_from_slice(&[v00, v10, v01]);
                    output.indices.extend_from_slice(&[v01, v10, v11]);
                }
            }
        }
    }
}

const CUBE_CORNERS: [[u32; 3]; 8] = [
    [0, 0, 0],
    [1, 0, 0],
    [0, 1, 0],
    [1, 1, 0],
    [0, 0, 1],
    [1, 0, 1],
    [0, 1, 1],
    [1, 1, 1],
];
const CUBE_CORNER_VECTORS: [Vec3A; 8] = [
    Vec3A::from_array([0.0, 0.0, 0.0]),
    Vec3A::from_array([1.0, 0.0, 0.0]),
    Vec3A::from_array([0.0, 1.0, 0.0]),
    Vec3A::from_array([1.0, 1.0, 0.0]),
    Vec3A::from_array([0.0, 0.0, 1.0]),
    Vec3A::from_array([1.0, 0.0, 1.0]),
    Vec3A::from_array([0.0, 1.0, 1.0]),
    Vec3A::from_array([1.0, 1.0, 1.0]),
];
const CUBE_EDGES: [[u32; 2]; 12] = [
    [0b000, 0b001],
    [0b000, 0b010],
    [0b000, 0b100],
    [0b001, 0b011],
    [0b001, 0b101],
    [0b010, 0b011],
    [0b010, 0b110],
    [0b011, 0b111],
    [0b100, 0b101],
    [0b100, 0b110],
    [0b101, 0b111],
    [0b110, 0b111],
];
