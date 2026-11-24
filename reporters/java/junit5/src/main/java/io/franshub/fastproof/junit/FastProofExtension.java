package io.franshub.fastproof.junit;

import io.franshub.fastproof.Attempt;

import java.lang.reflect.Executable;
import java.lang.reflect.Method;
import java.lang.reflect.Parameter;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

import org.junit.jupiter.api.extension.ExtensionContext;
import org.junit.jupiter.api.extension.InvocationInterceptor;
import org.junit.jupiter.api.extension.ReflectiveInvocationContext;

/**
 * Optional: reports the argument values of parameterized tests as parameters (with {@code -parameters} their real
 * names). Add it with {@code @ExtendWith(FastProofExtension.class)} or enable Jupiter extension autodetection
 * ({@code junit.jupiter.extensions.autodetection.enabled=true}); the listeners work without it.
 */
public final class FastProofExtension implements InvocationInterceptor {
    @Override
    public void interceptTestTemplateMethod(final Invocation<Void> invocation, final ReflectiveInvocationContext<Method> ctx,
            final ExtensionContext extension) throws Throwable {
        arguments(ctx);
        invocation.proceed();
    }

    @Override
    public <T> T interceptTestFactoryMethod(final Invocation<T> invocation, final ReflectiveInvocationContext<Method> ctx,
            final ExtensionContext extension) throws Throwable {
        arguments(ctx);
        return invocation.proceed();
    }

    private static void arguments(final ReflectiveInvocationContext<Method> ctx) {
        try {
            final Attempt a = Attempt.current();
            final Executable m = ctx.getExecutable();
            if (a == null || ctx.getArguments().isEmpty()) {
                return;
            }
            final Parameter[] names = m.getParameters();
            final List<String[]> params = new ArrayList<>();
            for (int i = 0; i < ctx.getArguments().size(); i++) {
                final Object v = ctx.getArguments().get(i);
                final String value = v instanceof Object[] arr ? Arrays.deepToString(arr) : String.valueOf(v);
                params.add(new String[] {i < names.length ? names[i].getName() : "arg" + i, value});
            }
            a.replaceParameters(params);
        } catch (RuntimeException | LinkageError e) {
            // naming parameters must never reach the test
        }
    }
}
