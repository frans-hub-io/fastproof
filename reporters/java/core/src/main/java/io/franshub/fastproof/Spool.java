package io.franshub.fastproof;

import java.io.IOException;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.security.SecureRandom;
import java.time.OffsetDateTime;
import java.time.ZoneOffset;
import java.util.HexFormat;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.stream.Stream;

/** The offline spool (format v1 in reporters/PROTOCOL.md); any FastProof reporter's {@code flush} sends it. */
final class Spool {
    private final Config cfg;
    private final String version;
    private final Path dir;
    private final Map<String, Object> meta = new LinkedHashMap<>();
    private long used;
    private boolean opened;
    private int written;
    private int dropped;

    Spool(final Config cfg, final String version) {
        this.cfg = cfg;
        this.version = version;
        final byte[] rnd = new byte[4];
        new SecureRandom().nextBytes(rnd);
        this.dir = cfg.spoolDir.resolve(Client.stamp() + "-" + ProcessHandle.current().pid() + "-" + HexFormat.of().formatHex(rnd));
    }

    synchronized boolean opened() {
        return opened;
    }

    synchronized int written() {
        return written;
    }

    synchronized int dropped() {
        return dropped;
    }

    Path dir() {
        return dir;
    }

    private boolean open() {
        if (opened) {
            return true;
        }
        try {
            Files.createDirectories(dir.resolve("blobs"));
            used = size(cfg.spoolDir);
            meta.put("v", 1);
            meta.put("reporter", "java/" + version);
            meta.put("project", cfg.project);
            meta.put("launch", cfg.launch);
            meta.put("createdAt", OffsetDateTime.now(ZoneOffset.UTC).toString());
            meta.put("pid", ProcessHandle.current().pid());
            String host = "unknown";
            try {
                host = InetAddress.getLocalHost().getHostName();
            } catch (IOException e) {
                // keep the default
            }
            meta.put("host", host);
            writeMeta();
            opened = true;
            return true;
        } catch (IOException e) {
            return false;
        }
    }

    private void writeMeta() throws IOException {
        final Path tmp = dir.resolve("meta.json.tmp");
        Files.writeString(tmp, Json.write(meta), StandardCharsets.UTF_8);
        Files.move(tmp, dir.resolve("meta.json"), StandardCopyOption.REPLACE_EXISTING);
    }

    private static long size(final Path root) {
        if (!Files.isDirectory(root)) {
            return 0;
        }
        try (Stream<Path> s = Files.walk(root)) {
            return s.filter(Files::isRegularFile).mapToLong(p -> {
                try {
                    return Files.size(p);
                } catch (IOException e) {
                    return 0;
                }
            }).sum();
        } catch (IOException e) {
            return 0;
        }
    }

    synchronized boolean addBlob(final String sha, final long size, final boolean passed, final Path path, final byte[] body) {
        if (!open()) {
            return false;
        }
        final Path target = dir.resolve("blobs").resolve(sha);
        if (Files.exists(target)) {
            return true;
        }
        final long limit = passed ? cfg.spoolMaxBytes / 2 : cfg.spoolMaxBytes;
        if (used + size > limit) {
            dropped++;
            return false;
        }
        try {
            final Path tmp = dir.resolve("blobs").resolve(sha + ".tmp");
            if (body != null) {
                Files.write(tmp, body);
            } else {
                Files.copy(path, tmp, StandardCopyOption.REPLACE_EXISTING);
            }
            Files.move(tmp, target, StandardCopyOption.REPLACE_EXISTING);
            used += size;
            return true;
        } catch (IOException e) {
            return false;
        }
    }

    synchronized boolean append(final Map<String, Object> result, final List<Map<String, Object>> blobs) {
        if (!open()) {
            return false;
        }
        final Map<String, Object> line = new LinkedHashMap<>();
        line.put("v", 1);
        line.put("result", result);
        line.put("blobs", blobs);
        final String text = Json.write(line) + "\n";
        if (used + text.length() > cfg.spoolMaxBytes) {
            dropped++;
            return false;
        }
        try {
            Client.append(dir.resolve("results.ndjson"), text);
            used += text.length();
            written++;
            return true;
        } catch (IOException e) {
            return false;
        }
    }

    synchronized void end(final String finish) {
        if (!opened) {
            return;
        }
        try {
            meta.put("ended", true);
            if (finish != null) {
                meta.put("finish", finish);
            }
            writeMeta();
        } catch (IOException e) {
            // the session stays sendable once its process is gone
        }
    }
}
