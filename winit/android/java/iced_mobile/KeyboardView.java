package iced_mobile;

import android.content.Context;
import android.text.Editable;
import android.text.Selection;
import android.text.SpannableStringBuilder;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.ViewParent;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;

/**
 * The view the keyboard types into: 1x1 pixels, transparent, focused while
 * a field wants the keyboard. It holds the keyboard's text, the mirror, and
 * reports it to the shell after each edit.
 *
 * <p>The mirror holds only what was typed since the last reset or rebase,
 * not the field's text. Everything here runs on the UI thread.
 */
final class KeyboardView extends View {
    /** The most UTF-16 units the mirror keeps when nothing is composing. */
    static final int CAP = 4096;

    private final InputMethodManager manager;
    private final SpannableStringBuilder mirror = new SpannableStringBuilder();

    /** False once uninstalled: then nothing here does anything. */
    private boolean installed = true;

    /** Whether a field wants the keyboard shown. */
    private boolean wanted;

    private int purpose = Keyboard.NORMAL;
    private int generation;

    /**
     * Counts the restarts. A connection created before the latest one
     * edits nothing: its keyboard still saw the text before it.
     */
    private int epoch;

    /** What the shell was told last. */
    private String reportedText = "";
    private int reportedSelStart;
    private int reportedSelEnd;
    private int reportedComposingStart = -1;
    private int reportedComposingEnd = -1;

    /** What the keyboard was told last. */
    private int sentSelStart;
    private int sentSelEnd;
    private int sentComposingStart = -1;
    private int sentComposingEnd = -1;

    KeyboardView(Context context) {
        super(context);

        manager = (InputMethodManager) context
                .getSystemService(Context.INPUT_METHOD_SERVICE);

        setImportantForAccessibility(IMPORTANT_FOR_ACCESSIBILITY_NO);
        setImportantForAutofill(IMPORTANT_FOR_AUTOFILL_NO);
        Selection.setSelection(mirror, 0);
    }

    Editable mirror() {
        return mirror;
    }

    /** Whether a connection created at {@code epoch} may still edit. */
    boolean accepts(int epoch) {
        return installed && epoch == this.epoch;
    }

    @Override
    public boolean onCheckIsTextEditor() {
        return true;
    }

    @Override
    public InputConnection onCreateInputConnection(EditorInfo info) {
        if (!installed) {
            return null;
        }

        // A new keyboard session starts with no composition.
        finishComposing();

        int options = EditorInfo.IME_ACTION_NONE
                | EditorInfo.IME_FLAG_NO_EXTRACT_UI
                | EditorInfo.IME_FLAG_NO_FULLSCREEN;

        switch (purpose) {
            case Keyboard.SECURE:
                info.inputType = EditorInfo.TYPE_CLASS_TEXT
                        | EditorInfo.TYPE_TEXT_VARIATION_PASSWORD;
                options |= EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING;
                break;
            case Keyboard.TERMINAL:
                info.inputType = EditorInfo.TYPE_CLASS_TEXT
                        | EditorInfo.TYPE_TEXT_FLAG_NO_SUGGESTIONS
                        | EditorInfo.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD;
                break;
            default:
                info.inputType = EditorInfo.TYPE_CLASS_TEXT;
                break;
        }

        info.imeOptions = options;
        info.initialSelStart = Selection.getSelectionStart(mirror);
        info.initialSelEnd = Selection.getSelectionEnd(mirror);
        info.initialCapsMode = 0;

        sentSelStart = info.initialSelStart;
        sentSelEnd = info.initialSelEnd;
        sentComposingStart = -1;
        sentComposingEnd = -1;

        return new KeyboardConnection(this, epoch);
    }

    @Override
    public void onWindowFocusChanged(boolean hasWindowFocus) {
        super.onWindowFocusChanged(hasWindowFocus);

        // showSoftInput fails while the window has no focus.
        if (hasWindowFocus && installed && wanted) {
            showLater();
        }
    }

    /**
     * Shows the keyboard once the messages already queued ran: the input
     * method manager then has started input for the view.
     */
    private void showLater() {
        post(new Runnable() {
            @Override
            public void run() {
                try {
                    if (installed && wanted && isFocused()) {
                        manager.showSoftInput(KeyboardView.this, 0);
                    }
                } catch (RuntimeException error) {
                    Keyboard.warn("showing the keyboard", error);
                }
            }
        });
    }

    void show(int purpose) {
        if (!installed) {
            return;
        }

        if (purpose != Keyboard.SECURE && purpose != Keyboard.TERMINAL) {
            purpose = Keyboard.NORMAL;
        }

        boolean changed = purpose != this.purpose;
        this.purpose = purpose;
        wanted = true;

        if (!isFocused()) {
            // Focusable from the first field on: before it, the view does
            // not take the focus when a hardware key leaves touch mode.
            setFocusable(true);
            setFocusableInTouchMode(true);

            // Starts input, which reads the purpose.
            requestFocus();
        } else if (changed) {
            // Another field: its keyboard does not see this one's text.
            if (!isEmpty()) {
                rebase();
            }

            restart();
        }

        if (!manager.showSoftInput(this, 0)) {
            showLater();
        }
    }

    void hide() {
        if (!installed) {
            return;
        }

        wanted = false;
        manager.hideSoftInputFromWindow(getWindowToken(), 0);
    }

    void reset(int generation) {
        if (!installed) {
            return;
        }

        this.generation = generation;
        clear();
        restart();
    }

    void uninstall() {
        if (!installed) {
            return;
        }

        boolean focused = isFocused();

        installed = false;
        wanted = false;
        epoch++;
        clear();

        if (focused) {
            manager.hideSoftInputFromWindow(getWindowToken(), 0);
        }

        ViewParent parent = getParent();

        if (parent instanceof ViewGroup) {
            ((ViewGroup) parent).removeView(this);
        }
    }

    /**
     * Tells the shell about a key the mirror does not handle, after the text
     * typed before it: a composition is finished and reported first.
     */
    void forward(int keyCode) {
        if (!installed) {
            return;
        }

        finishComposing();
        Keyboard.key(generation, keyCode);

        // The app's caret moved where the mirror cannot follow: the text
        // starts again there.
        if (moves(keyCode) && !isEmpty()) {
            rebase();
        }
    }

    /**
     * Reports the mirror to the shell if it changed since the last report,
     * and its selection to the keyboard.
     */
    void report() {
        if (!installed) {
            return;
        }

        int selStart = Selection.getSelectionStart(mirror);
        int selEnd = Selection.getSelectionEnd(mirror);
        int composingStart = composingStart();
        int composingEnd = composingEnd();

        boolean changed = selStart != reportedSelStart
                || selEnd != reportedSelEnd
                || composingStart != reportedComposingStart
                || composingEnd != reportedComposingEnd
                || !reportedText.contentEquals(mirror);

        if (changed) {
            String text = mirror.toString();

            reportedText = text;
            reportedSelStart = selStart;
            reportedSelEnd = selEnd;
            reportedComposingStart = composingStart;
            reportedComposingEnd = composingEnd;

            Keyboard.state(generation, text, selStart, selEnd, composingStart,
                    composingEnd);

            // A password is not kept here, and the mirror stays small. A
            // composition is kept until the keyboard commits it.
            boolean composing = composingStart != -1;
            boolean clear = purpose == Keyboard.SECURE
                    ? text.length() > 0
                    : text.length() > CAP;

            if (clear && !composing) {
                rebase();
                return;
            }
        }

        sendSelection();
    }

    private void finishComposing() {
        if (composingStart() != -1) {
            BaseInputConnection.removeComposingSpans(mirror);
        }

        report();
    }

    /** Clears the mirror and tells the shell, then the keyboard. */
    private void rebase() {
        clear();
        Keyboard.rebase(generation);
        sendSelection();
    }

    /** Clears the mirror, which the shell knows to be empty. */
    private void clear() {
        mirror.clear();
        mirror.clearSpans();
        Selection.setSelection(mirror, 0);

        reportedText = "";
        reportedSelStart = 0;
        reportedSelEnd = 0;
        reportedComposingStart = -1;
        reportedComposingEnd = -1;
    }

    /**
     * Has the keyboard start over with the current purpose and mirror. The
     * connections it had edit nothing from now on.
     */
    private void restart() {
        epoch++;

        sentSelStart = Selection.getSelectionStart(mirror);
        sentSelEnd = Selection.getSelectionEnd(mirror);
        sentComposingStart = -1;
        sentComposingEnd = -1;

        manager.restartInput(this);
    }

    /** Tells the keyboard the selection if it changed since it was told. */
    private void sendSelection() {
        int selStart = Selection.getSelectionStart(mirror);
        int selEnd = Selection.getSelectionEnd(mirror);
        int composingStart = composingStart();
        int composingEnd = composingEnd();

        if (selStart == sentSelStart && selEnd == sentSelEnd
                && composingStart == sentComposingStart
                && composingEnd == sentComposingEnd) {
            return;
        }

        sentSelStart = selStart;
        sentSelEnd = selEnd;
        sentComposingStart = composingStart;
        sentComposingEnd = composingEnd;

        manager.updateSelection(this, selStart, selEnd, composingStart,
                composingEnd);
    }

    private boolean isEmpty() {
        return mirror.length() == 0 && composingStart() == -1;
    }

    /** The composing region's start, or -1 when nothing is composing. */
    private int composingStart() {
        int start = BaseInputConnection.getComposingSpanStart(mirror);
        int end = BaseInputConnection.getComposingSpanEnd(mirror);

        if (start < 0 || end < 0 || start == end) {
            return -1;
        }

        return Math.min(start, end);
    }

    /** The composing region's end, or -1 when nothing is composing. */
    private int composingEnd() {
        int start = BaseInputConnection.getComposingSpanStart(mirror);
        int end = BaseInputConnection.getComposingSpanEnd(mirror);

        if (start < 0 || end < 0 || start == end) {
            return -1;
        }

        return Math.max(start, end);
    }

    /** Whether the key moves the app's caret. */
    private static boolean moves(int keyCode) {
        switch (keyCode) {
            case KeyEvent.KEYCODE_DPAD_UP:
            case KeyEvent.KEYCODE_DPAD_DOWN:
            case KeyEvent.KEYCODE_DPAD_LEFT:
            case KeyEvent.KEYCODE_DPAD_RIGHT:
            case KeyEvent.KEYCODE_MOVE_HOME:
            case KeyEvent.KEYCODE_MOVE_END:
                return true;
            default:
                return false;
        }
    }
}
