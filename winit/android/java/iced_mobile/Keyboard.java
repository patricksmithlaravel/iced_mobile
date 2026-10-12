package iced_mobile;

import android.app.Activity;
import android.content.Context;
import android.util.Log;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.InputMethodManager;

/**
 * The shell's handle on the system keyboard for one Activity: what
 * iced_winit calls, and the native methods it registers.
 *
 * <p>The shell calls {@link #install}, {@link #show}, {@link #hide},
 * {@link #reset} and {@link #uninstall} on android_main's thread. Each posts
 * its work to the UI thread and returns at once, so the work runs in the
 * order of the calls. The native methods are called on the UI thread only.
 * Every native call carries the generation the last {@link #reset} adopted
 * (0 before the first one), and none is made with an older generation once
 * that reset has run.
 *
 * <p>Nothing here logs the keyboard's text.
 */
public final class Keyboard {
    static final String TAG = "iced_mobile";

    /** {@link #show}'s purposes, iced's {@code input_method::Purpose}. */
    static final int NORMAL = 0;
    static final int SECURE = 1;
    static final int TERMINAL = 2;

    private static final int CREATE = 0;
    private static final int SHOW = 1;
    private static final int HIDE = 2;
    private static final int RESET = 3;
    private static final int UNINSTALL = 4;

    /** Cleared on the UI thread by {@link #uninstall}. */
    private volatile Activity activity;

    /** UI thread only. */
    private KeyboardView view;

    /**
     * UI thread only: the view could not be added, so {@link #show} and
     * {@link #hide} fall back to the window's decor view, as the shell does
     * without this class.
     */
    private boolean failed;

    /** UI thread only: a native call failed and was logged. */
    private static boolean nativeWarned;

    private Keyboard(Activity activity) {
        this.activity = activity;
    }

    /**
     * Creates the keyboard of {@code activity}. Its view is created and added
     * to the Activity on the UI thread.
     */
    public static Keyboard install(Activity activity) {
        if (activity == null) {
            throw new IllegalArgumentException("no Activity");
        }

        Keyboard keyboard = new Keyboard(activity);
        keyboard.post(CREATE, 0);
        return keyboard;
    }

    /**
     * Shows the keyboard for a field with {@code purpose}: 0 normal,
     * 1 secure, 2 terminal.
     */
    public void show(int purpose) {
        post(SHOW, purpose);
    }

    /** Hides the keyboard. */
    public void hide() {
        post(HIDE, 0);
    }

    /**
     * Forgets the keyboard's text and adopts {@code generation} for every
     * later native call.
     */
    public void reset(int generation) {
        post(RESET, generation);
    }

    /** Removes the view and lets go of the Activity. */
    public void uninstall() {
        post(UNINSTALL, 0);
    }

    private void post(int op, int argument) {
        Activity activity = this.activity;

        if (activity != null) {
            activity.runOnUiThread(new Task(op, argument));
        }
    }

    /** One call's work on the UI thread. */
    private final class Task implements Runnable {
        private final int op;
        private final int argument;

        Task(int op, int argument) {
            this.op = op;
            this.argument = argument;
        }

        @Override
        public void run() {
            try {
                switch (op) {
                    case CREATE:
                        create();
                        break;
                    case SHOW:
                        if (view != null) {
                            view.show(argument);
                        } else if (failed) {
                            decor(true);
                        }
                        break;
                    case HIDE:
                        if (view != null) {
                            view.hide();
                        } else if (failed) {
                            decor(false);
                        }
                        break;
                    case RESET:
                        if (view != null) {
                            view.reset(argument);
                        }
                        break;
                    case UNINSTALL:
                        destroy();
                        break;
                    default:
                        break;
                }
            } catch (Throwable error) {
                Log.w(TAG, "Keyboard: a call from the shell failed", error);
            }
        }
    }

    private void create() {
        Activity activity = this.activity;

        if (activity == null || view != null || failed) {
            return;
        }

        try {
            KeyboardView view = new KeyboardView(activity);
            activity.addContentView(view, new ViewGroup.LayoutParams(1, 1));
            this.view = view;
        } catch (RuntimeException | LinkageError error) {
            failed = true;
            Log.w(TAG, "Keyboard: the input view cannot be added; the "
                    + "keyboard can only send key events", error);
        }
    }

    private void destroy() {
        KeyboardView view = this.view;
        this.view = null;

        if (view != null) {
            view.uninstall();
        }

        activity = null;
    }

    /** Shows or hides the keyboard for the window's decor view. */
    private void decor(boolean show) {
        Activity activity = this.activity;

        if (activity == null || activity.getWindow() == null) {
            return;
        }

        View decor = activity.getWindow().getDecorView();
        InputMethodManager manager = (InputMethodManager) activity
                .getSystemService(Context.INPUT_METHOD_SERVICE);

        if (manager == null) {
            return;
        }

        if (show) {
            manager.showSoftInput(decor, 0);
        } else {
            manager.hideSoftInputFromWindow(decor.getWindowToken(), 0);
        }
    }

    /** UI thread: the keyboard's text, selection and composing region. */
    static void state(int generation, String text, int selStart, int selEnd,
            int composingStart, int composingEnd) {
        try {
            nativeState(generation, text, selStart, selEnd, composingStart,
                    composingEnd);
        } catch (Throwable error) {
            nativeFailed(error);
        }
    }

    /** UI thread: a key the keyboard pressed that the text does not hold. */
    static void key(int generation, int keyCode) {
        try {
            nativeKey(generation, keyCode);
        } catch (Throwable error) {
            nativeFailed(error);
        }
    }

    /** UI thread: the text was cleared where the app's caret is. */
    static void rebase(int generation) {
        try {
            nativeRebase(generation);
        } catch (Throwable error) {
            nativeFailed(error);
        }
    }

    /** UI thread: logs an exception that is not the keyboard's text. */
    static void warn(String what, Throwable error) {
        Log.w(TAG, "Keyboard: " + what + " failed", error);
    }

    private static void nativeFailed(Throwable error) {
        if (!nativeWarned) {
            nativeWarned = true;
            Log.w(TAG, "Keyboard: the shell cannot be told what the "
                    + "keyboard typed", error);
        }
    }

    private static native void nativeState(int generation, String text,
            int selStart, int selEnd, int composingStart, int composingEnd);

    private static native void nativeKey(int generation, int keyCode);

    private static native void nativeRebase(int generation);
}
