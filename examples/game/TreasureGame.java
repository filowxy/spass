import java.io.*;
import java.net.*;
import java.util.*;

/**
 * Treasure Hunt — Java front end (section 1.6).
 *
 * The Spass program (examples/game) is the game's logic core: it owns the
 * hunter position, the treasure, movement rules, world bounds, the distance
 * hint and win detection, running as a resident world inside the saple
 * runner. This front end only renders the grid, reads player input and
 * shuttles values through the runner's set/get protocol.
 *
 * Usage:
 *   java TreasureGame <projectDir> <port>          interactive
 *   java TreasureGame <projectDir> <port> auto     scripted win (E2E)
 */
public class TreasureGame {
    static String host = "127.0.0.1";
    static int port = 9100;
    static String project;

    /** Persistent connection to the runner — one long connection reused
     * for every RPC (the runner serves many frames per connection).
     * Short connections exhaust Windows ephemeral ports fast. */
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

    /** One framed JSON RPC to the runner (4-byte big-endian length + body).
     * Windows can contend ephemeral ports when a turn poll opens many short
     * connections in a row, so a transient BindException is retried briefly
     * (`ponytail`: a pooled long connection is the real fix). */
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

    /** Extract a numeric field from a flat JSON response (no deps). */
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

    static void setFile(String channel, double value) throws IOException {
        rpc("{\"cmd\":\"set\",\"project\":\"" + project + "\",\"channel\":\"" + channel
            + "\",\"value\":" + value + "}");
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
        // Clear stale disk slots from a previous world — the fresh world
        // publishes from steps=1, which would never exceed an old value.
        File outDir = new File(project, "data/output");
        File[] stale = outDir.listFiles();
        if (stale != null) for (File f : stale) f.delete();
    }

    /** Advance one turn: push the action, wait until `steps` CHANGES. */
    static void turn(int action) throws IOException, InterruptedException {
        double before = get("steps") == null ? -1 : get("steps");
        setFile("cmd_action", action);
        setSocket("tick", System.currentTimeMillis() % 100000);
        double steps = before;
        int polls = 0;
        while (steps == before) {
            Thread.sleep(30);
            Double s = get("steps");
            if (s != null) steps = s;
            if (++polls > 40) {
                throw new RuntimeException("world did not advance (before=" + before + ")");
            }
        }
    }

    static void render(Double hx, Double hy, Double steps, Double found, Double dist) {
        if (hx == null || hy == null || steps == null || found == null) {
            System.out.println("(waiting for world state...)");
            return;
        }
        char[][] grid = new char[10][10];
        for (char[] row : grid) Arrays.fill(row, '.');
        int x = (int) Math.round(hx), y = (int) Math.round(hy);
        if (x >= 0 && x < 10 && y >= 0 && y < 10) grid[y][x] = '@';
        System.out.println("+----------+  steps=" + (int) Math.round(steps)
                           + "  heat=" + (dist == null ? "?" : (dist < 1.5 ? "SCORCHING"
                                           : dist < 3 ? "hot" : dist < 5 ? "warm" : "cold")));
        for (char[] row : grid) System.out.println("|" + new String(row) + "|");
        System.out.println("+----------+  @ = you (column " + x + ", row " + y + ")");
    }

    public static void main(String[] args) throws Exception {
        project = new File(args[0]).getCanonicalPath().replace('\\', '/');
        port = Integer.parseInt(args[1]);
        boolean auto = args.length > 2 && args[2].equals("auto");

        startWorld();
        // Kick turn 0 so the world publishes its initial state.
        setFile("cmd_action", 9.0);
        setSocket("tick", 1);
        Thread.sleep(300);

        // Fixed treasure column/row: the front end does NOT know it — the
        // auto play walks a scripted path and watches `found`.
        int[] autoMoves = {3, 3, 3, 3, 3, 3, 3, 1, 1, 1};

        if (auto) {
            for (int move : autoMoves) {
                turn(move);
            }
        }

        while (true) {
            Double hx = get("hx"), hy = get("hy"), steps = get("steps");
            Double found = get("found"), dist = get("dist");
            render(hx, hy, steps, found, dist);
            if (found != null && found == 1.0) {
                System.out.println("*** TREASURE FOUND in " + (int) Math.round(steps)
                                   + " steps — logic core: Spass, front end: Java ***");
                return;
            }
            int action;
            if (auto) {
                action = autoMoves[Math.min(autoMoves.length - 1,
                       (int) Math.round(steps == null ? 0 : steps))];
            } else {
                System.out.print("move [W=up S=down A=left D=right, Q=quit] > ");
                System.out.flush();
                String line = new BufferedReader(new InputStreamReader(System.in)).readLine();
                if (line == null || line.isEmpty()) continue;
                char c = Character.toUpperCase(line.trim().charAt(0));
                if (c == 'Q') return;
                action = "WSDA".indexOf(c);
                if (action < 0) continue;
            }
            turn(action);
        }
    }
}
