package io.franshub.fastproof;

import java.util.List;
import java.util.Map;

/** The reporter's JSON helpers for tests in other packages. */
public final class TestJson {
    private TestJson() {
    }

    public static String write(final Object v) {
        return Json.write(v);
    }

    public static Object parse(final String s) {
        return Json.parse(s);
    }

    public static Map<String, Object> object(final Object v) {
        return Json.object(v);
    }

    public static List<Object> array(final Object v) {
        return Json.array(v);
    }
}
