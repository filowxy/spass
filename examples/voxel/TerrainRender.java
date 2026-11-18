import java.io.*;
import java.net.*;
import java.util.*;

/**
 * Voxel terrain renderer (3D).
 *
 * Spass (examples/voxel) generates an 8x8x8 voxel volume as a resident
 * world: terrain surface (layered sines), materials (air/stone/water/
 * grass), a vec3-carved cave, and column packing (pow(8, y)). This front
 * end consumes the 64 packed columns and renders 8 depth slices.
 *
 * Usage: java VoxelTerrain <projectDir> <port>
 */
public class TerrainRender {}
