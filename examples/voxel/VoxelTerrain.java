import java.io.*;
import java.net.*;
import java.util.*;

/**
 * Voxel terrain renderer (section 1.6 — 3D).
 *
 * Spass (examples/voxel) generates an 8x8x8 voxel volume as a resident
 * world: terrain surface (layered sines), materials (air/stone/water/
 * grass), a vec3-carved cave, and column packing (pow(8, y)). This front
 * end consumes the 64 packed columns and renders 8 depth slices.
 *
 * Usage: java VoxelTerrain <projectDir> <port>
 */
public class VoxelTerrain {
    static String host = "127.0.0.1";
    static int port = 9100;
    static String project;

    static Socket sock;
    static DataOutputStream out;
    static DataInputStream in;

    static void ensureConn() throws IOException {
        if (sock != null && sock.isConnected() && !sock.isClosed()) return;
        sock = new Socket();
        sock.connect(new InetSocketAddress(host, port), 2000);
        out = new DataOutputStream(new BufferedOutputStream(sock.getOutputStream()));
        in = new DataInputStream(new BufferedInputStream(sock.getInputStream()));
    }

    static void closeConn() {
        try { if (sock != null) sock.close(); } catch (IOException ignored) {}
        sock = null;
    }

    static String rpc(String json) throws IOException {
        IOException last = null;
        for (int attempt = 0; attempt < 3; attempt++) {
            try {
                ensureConn();
                byte[] body = json.getBytes("UTF-8");
                out.writeInt(body.length); // big-endian, matches section 9.2
                out.write(body);
                out.flush();
                int len = in.readInt();
                byte[] resp = new byte[len];
                in.readFully(resp);
                return new String(resp, "UTF-8");
            } catch (IOException e) {
                closeConn();
                last = e;
            }
        }
        throw last;
    }

    static double num(String json, String field) {
        String key = "\"" + field + "\":";
        int i = json.indexOf(key);
        if (i < 0) throw new RuntimeException("field missing: " + field + " in " + json);
        i += key.length();
        int j = i;
        while (j < json.length() &&
               (Character.isDigit(json.charAt(j)) || "-+.eE".indexOf(json.charAt(j)) >= 0)) j++;
        return Double.parseDouble(json.substring(i, j));
    }

    static void setSocket(String channel, double value) throws IOException {
        rpc("{\"cmd\":\"set\",\"project\":\"" + project + "\",\"channel\":\"" + channel
            + "\",\"value\":" + value + ",\"mode\":\"socket\"}");
    }

    static Double get(String channel) throws IOException {
        String r = rpc("{\"cmd\":\"get\",\"project\":\"" + project + "\",\"channel\":\""
                       + channel + "\"}");
        if (!r.contains("\"found\":true")) return null;
        return num(r, "value");
    }

    static void startWorld() throws IOException {
        String r = rpc("{\"cmd\":\"start\",\"project\":\"" + project + "\"}");
        if (!r.contains("\"ok\":true")) throw new RuntimeException("start failed: " + r);
        File outDir = new File(project, "data/output");
        File[] stale = outDir.listFiles();
        if (stale != null) for (File f : stale) f.delete();
    }

    static char voxelChar(int code) {
        // 0=air 1=stone 2=water 3=grass
        switch (code) {
            case 1: return '%';
            case 2: return '~';
            case 3: return '#';
            default: return ' ';
        }
    }

    public static void main(String[] args) throws Exception {
        project = new File(args[0]).getCanonicalPath().replace('\\', '/');
        port = Integer.parseInt(args[1]);

        startWorld();
        int[][][] vox = new int[8][8][8]; // [z][y][x]
        int last = -1;

        // Consume 64 packed columns: the world publishes (idx, col) then
        // waits for our ack — we ack with the index we consumed.
        for (int ack = 0; ack < 64; ack++) {
            double idx;
            int polls = 0;
            while (true) {
                Double i = get("vox_idx");
                if (i != null && (int) Math.round(i) == ack) { idx = i; break; }
                if (++polls > 200) throw new RuntimeException(
                    "no column " + ack + " from the world (last=" + i + ")");
                Thread.sleep(25);
            }
            double col = get("vox_col");
            int x = ack % 8;
            int z = ack / 8;
            for (int y = 0; y < 8; y++) {
                // Unpack 3 bits per voxel: code_y = floor(col / 8^y) mod 8.
                double div = Math.pow(8, y);
                vox[z][y][x] = (int) Math.floor(col / div) % 8;
            }
            setSocket("ack", ack);
            last = ack;
        }
        for (int i = 0; i < 100; i++) {
            Double done = get("gen3d_done");
            if (done != null && done >= 63.5) break;
            Thread.sleep(25);
        }

        System.out.println("voxel terrain generated by Spass: " + (last + 1)
                           + " columns (8x8x8 volume)");
        System.out.println("legend: % stone  # grass  ~ water  (space) air\n");
        // Render 8 depth slices: each slice is the x-y face at depth z.
        for (int z = 0; z < 8; z++) {
            System.out.println("slice z=" + z + ":");
            System.out.println("  +--------+  y=7 (top)");
            for (int y = 7; y >= 0; y--) {
                StringBuilder row = new StringBuilder("  |");
                for (int x = 0; x < 8; x++) {
                    row.append(voxelChar(vox[z][y][x]));
                }
                row.append('|');
                System.out.println(row);
            }
            System.out.println("  +--------+  y=0 (bedrock)\n");
        }
        closeConn();
    }
}
