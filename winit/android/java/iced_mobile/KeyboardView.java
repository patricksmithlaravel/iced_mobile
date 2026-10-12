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
 * The view the keyboard types into: 1x1 pixels, transparent, focused from
 * the first field that wants the keyboard on. It holds the keyboard's text,
 * the mirror, and reports it to the shell after each edit.
 *
 * <p>It is a text editor only while a field wants the keyboard: otherwise
 * the keyboard sees a view that edits nothing, as without this view, and
 * lets a hardware keyboard's keys through to the app.
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

    /** The shell asked to forget the text once nothing composes. */
    private boolean forgetting;

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
        return installed && wanted;
    }

    @Override
    public InputConnection onCreateInputConnection(EditorInfo info) {
        // No field: the keyboard gets no connection and an editor of type
        // TYPE_NULL, as for a view that is not a text editor.
        if (!installed || !wanted) {
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

        boolean focused = isFocused();
        boolean changed = purpose != this.purpose;

        if (focused && wanted && changed) {
            // Another purpose, maybe another field: a composition is
            // committed where it is, under the purpose it was typed with,
            // and the keyboard for the new purpose does not see this text.
            finishComposing();

            if (!isEmpty()) {
                rebase();
            }
        }

        // The keyboard edits nothing since hide(), or edits for the old
        // purpose: it starts again with this one.
        boolean restart = focused && (!wanted || changed);

        this.purpose = purpose;
        wanted = true;

        if (!focused) {
            // Focusable from the first field on: before it, the view does
            // not take the focus when a hardware key leaves touch mode.
            setFocusable(true);
            setFocusableInTouchMode(true);

            // Starts input, which reads the purpose.
            requestFocus();
        } else if (restart) {
            restart();
        }

        if (!manager.showSoftInput(this, 0)) {
            showLater();
        }
    }

    /**
     * Hides the keyboard and forgets the mirror, which the shell has reset
     * or knows to hold no text. The view keeps the focus but is no text
     * editor until the next show.
     */
    void hide() {
        if (!installed) {
            return;
        }

        boolean editing = wanted;

        wanted = false;
        manager.hideSoftInputFromWindow(getWindowToken(), 0);
        clear();

        if (editing && isFocused()) {
            restart();
        }
    }

    /**
     * Forgets the mirror's text, which stays in the field (a rebase), unless
     * a reset ran since the shell asked at {@code generation}. A composition
     * is kept until it ends. An empty mirror is rebased too, so the shell
     * always learns that its request ran.
     *
     * <p>This can run between two calls of the keyboard's batch edit, whose
     * edits so far are not reported yet: they are reported first, as
     * {@link #forward} reports them, so that the rebase loses none.
     */
    void forget(int generation) {
        if (!installed || generation != this.generation) {
            return;
        }

        forgetting = true;
        report();
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

        // The app's caret moved, or its field may have been submitted or
        // left, where the mirror cannot follow: the text starts again at the
        // app's caret, with the same generation, so what the keyboard types
        // next counts.
        if (startsOver(keyCode) && !isEmpty()) {
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

        boolean secure = purpose == Keyboard.SECURE;
        int selStart = Selection.getSelectionStart(mirror);
        int selEnd = Selection.getSelectionEnd(mirror);
        boolean composing = composingStart() != -1;

        // A password field never shows a composition: the shell gets it as
        // typed text, which the field masks.
        int composingStart = secure ? -1 : composingStart();
        int composingEnd = secure ? -1 : composingEnd();

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
        }

        // The keyboard sees where its edit left the cursor before a rebase
        // moves it to the start, so a keyboard that keeps the text before
        // its cursor learns that it is gone.
        sendSelection();

        // A password is not kept here, the mirror stays small, and the text
        // the shell asked to forget goes, even when the mirror is empty. A
        // composition is kept until the keyboard commits it.
        boolean clear = forgetting
                || (secure ? mirror.length() > 0 : mirror.length() > CAP);

        if (clear && !composing) {
            rebase();
        }
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
        forgetting = false;

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

    /**
     * Whether the mirror starts over after the key: it moves the app's
     * caret, or may submit or leave the field.
     */
    private static boolean startsOver(int keyCode) {
        switch (keyCode) {
            case KeyEvent.KEYCODE_DPAD_UP:
            case KeyEvent.KEYCODE_DPAD_DOWN:
            case KeyEvent.KEYCODE_DPAD_LEFT:
            case KeyEvent.KEYCODE_DPAD_RIGHT:
            case KeyEvent.KEYCODE_MOVE_HOME:
            case KeyEvent.KEYCODE_MOVE_END:
            case KeyEvent.KEYCODE_ENTER:
            case KeyEvent.KEYCODE_NUMPAD_ENTER:
            case KeyEvent.KEYCODE_TAB:
            case KeyEvent.KEYCODE_ESCAPE:
                return true;
            default:
                return false;
        }
    }
}
