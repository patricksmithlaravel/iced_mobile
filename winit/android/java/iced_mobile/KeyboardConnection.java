package iced_mobile;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.text.Editable;
import android.text.Selection;
import android.text.TextUtils;
import android.view.KeyCharacterMap;
import android.view.KeyEvent;
import android.view.inputmethod.BaseInputConnection;

/**
 * What the keyboard does to the view's mirror. The editing itself is
 * {@link BaseInputConnection}'s; this class counts batch edits, has the view
 * report the mirror when the outermost one ends, handles the keys the
 * keyboard sends, the Enter and Tab it commits and the actions of its
 * editing panel (Select all, Cut, Copy, Paste), and keeps line breaks and
 * tabs out of what it commits and composes (replaceText is not filtered).
 *
 * <p>A connection edits nothing once the view restarted the keyboard, was
 * uninstalled, or the system closed the connection. Everything here runs on
 * the UI thread.
 */
final class KeyboardConnection extends BaseInputConnection {
    private final KeyboardView view;
    private final int epoch;
    private int batches;
    private boolean closed;

    KeyboardConnection(KeyboardView view, int epoch) {
        super(view, true);
        this.view = view;
        this.epoch = epoch;
    }

    private boolean active() {
        return !closed && view.accepts(epoch);
    }

    @Override
    public Editable getEditable() {
        return active() ? view.mirror() : null;
    }

    @Override
    public boolean beginBatchEdit() {
        if (!active()) {
            return false;
        }

        batches++;
        return true;
    }

    @Override
    public boolean endBatchEdit() {
        if (!active()) {
            return false;
        }

        if (batches > 0) {
            batches--;
        }

        if (batches == 0) {
            view.report();
        }

        return batches > 0;
    }

    /**
     * Ends the connection. A composition stays: the view finishes it when
     * the keyboard starts over, and the system when the focus moves, while
     * a newer connection may be composing already.
     */
    @Override
    public void closeConnection() {
        boolean pending = active() && batches > 0;

        closed = true;
        batches = 0;

        // Edits of a batch that will not end.
        if (pending) {
            view.report();
        }

        // Its finishComposingText does nothing now.
        super.closeConnection();
    }

    @Override
    public boolean commitText(CharSequence text, int newCursorPosition) {
        if (!active()) {
            return false;
        }

        // What keyboards commit for Enter or Tab: a key for the app, not
        // text.
        int key = key(text);

        if (key != 0) {
            view.forward(key);
            return true;
        }

        beginBatchEdit();

        try {
            super.commitText(plain(text), newCursorPosition);
        } catch (RuntimeException error) {
            Keyboard.warn("commitText", error);
        } finally {
            endBatchEdit();
        }

        return true;
    }

    @Override
    public boolean setComposingText(CharSequence text, int newCursorPosition) {
        if (!active()) {
            return false;
        }

        beginBatchEdit();

        try {
            super.setComposingText(plain(text), newCursorPosition);
        } catch (RuntimeException error) {
            Keyboard.warn("setComposingText", error);
        } finally {
            endBatchEdit();
        }

        return true;
    }

    @Override
    public boolean setComposingRegion(int start, int end) {
        if (!active()) {
            return false;
        }

        beginBatchEdit();

        try {
            super.setComposingRegion(start, end);
        } catch (RuntimeException error) {
            Keyboard.warn("setComposingRegion", error);
        } finally {
            endBatchEdit();
        }

        return true;
    }

    @Override
    public boolean finishComposingText() {
        if (!active()) {
            return false;
        }

        beginBatchEdit();

        try {
            super.finishComposingText();
        } catch (RuntimeException error) {
            Keyboard.warn("finishComposingText", error);
        } finally {
            endBatchEdit();
        }

        return true;
    }

    @Override
    public boolean setSelection(int start, int end) {
        if (!active()) {
            return false;
        }

        beginBatchEdit();

        try {
            super.setSelection(start, end);
        } catch (RuntimeException error) {
            Keyboard.warn("setSelection", error);
        } finally {
            endBatchEdit();
        }

        return true;
    }

    @Override
    public boolean deleteSurroundingText(int beforeLength, int afterLength) {
        if (!active()) {
            return false;
        }

        deleteAround(beforeLength, afterLength, false,
                "deleteSurroundingText");
        return true;
    }

    @Override
    public boolean deleteSurroundingTextInCodePoints(int beforeLength,
            int afterLength) {
        if (!active()) {
            return false;
        }

        deleteAround(beforeLength, afterLength, true,
                "deleteSurroundingTextInCodePoints");
        return true;
    }

    /**
     * The keyboard's Select all, Cut, Copy and Paste, as from its editing
     * panel. Select all is the field's own, which selects the field's text
     * (the mirror holds only some of it), and the mirror starts over. Cut
     * and Copy take the mirror's selection, the only one the keyboard
     * knows of, and never a password. Paste inserts the clipboard's text as
     * a key's character goes in, without line breaks and tabs.
     */
    @Override
    public boolean performContextMenuAction(int id) {
        if (!active()) {
            return false;
        }

        try {
            switch (id) {
                case android.R.id.selectAll:
                    view.forward(Keyboard.SELECT_ALL);
                    return true;
                case android.R.id.cut:
                    return copy(true);
                case android.R.id.copy:
                    return copy(false);
                case android.R.id.paste:
                case android.R.id.pasteAsPlainText:
                    return paste();
                default:
                    return false;
            }
        } catch (RuntimeException error) {
            Keyboard.warn("performContextMenuAction", error);
            return false;
        }
    }

    @Override
    public boolean performEditorAction(int editorAction) {
        if (!active()) {
            return false;
        }

        view.forward(KeyEvent.KEYCODE_ENTER);
        return true;
    }

    /**
     * The keyboard's keys go to the shell here, not through the view root:
     * there they would reach the app on another path than the text, and an
     * Enter could overtake the text typed before it.
     */
    @Override
    @SuppressWarnings("deprecation")
    public boolean sendKeyEvent(KeyEvent event) {
        if (!active() || event == null) {
            return false;
        }

        try {
            if (event.getAction() == KeyEvent.ACTION_MULTIPLE) {
                String characters = event.getCharacters();

                if (event.getKeyCode() == KeyEvent.KEYCODE_UNKNOWN
                        && characters != null && !characters.isEmpty()) {
                    type(characters);
                }

                return true;
            }

            if (event.getAction() != KeyEvent.ACTION_DOWN) {
                return true;
            }

            int keyCode = event.getKeyCode();

            switch (keyCode) {
                case KeyEvent.KEYCODE_DEL:
                    if (!deleteBackward()) {
                        view.forward(keyCode);
                    }
                    break;
                case KeyEvent.KEYCODE_FORWARD_DEL:
                    if (!deleteForward()) {
                        view.forward(keyCode);
                    }
                    break;
                case KeyEvent.KEYCODE_ENTER:
                case KeyEvent.KEYCODE_NUMPAD_ENTER:
                case KeyEvent.KEYCODE_TAB:
                case KeyEvent.KEYCODE_ESCAPE:
                case KeyEvent.KEYCODE_DPAD_UP:
                case KeyEvent.KEYCODE_DPAD_DOWN:
                case KeyEvent.KEYCODE_DPAD_LEFT:
                case KeyEvent.KEYCODE_DPAD_RIGHT:
                case KeyEvent.KEYCODE_MOVE_HOME:
                case KeyEvent.KEYCODE_MOVE_END:
                    view.forward(keyCode);
                    break;
                default:
                    // Not a shortcut, and not a dead key.
                    int character = event.isCtrlPressed()
                            || event.isMetaPressed()
                            ? 0
                            : event.getUnicodeChar(event.getMetaState());

                    if (character > 0
                            && (character & KeyCharacterMap.COMBINING_ACCENT)
                                    == 0) {
                        type(new String(Character.toChars(character)));
                    }
                    break;
            }
        } catch (RuntimeException error) {
            Keyboard.warn("sendKeyEvent", error);
        }

        return true;
    }

    @Override
    public CharSequence getTextBeforeCursor(int length, int flags) {
        if (!active() || length < 0) {
            return null;
        }

        try {
            return super.getTextBeforeCursor(length, flags);
        } catch (RuntimeException error) {
            Keyboard.warn("getTextBeforeCursor", error);
            return null;
        }
    }

    @Override
    public CharSequence getTextAfterCursor(int length, int flags) {
        if (!active() || length < 0) {
            return null;
        }

        try {
            // Older releases add the length to the cursor unchecked.
            return super.getTextAfterCursor(
                    Math.min(length, view.mirror().length()), flags);
        } catch (RuntimeException error) {
            Keyboard.warn("getTextAfterCursor", error);
            return null;
        }
    }

    /**
     * Characters from a key: committed at the cursor after any composition,
     * as a hardware key types into a text field.
     */
    private void type(String characters) {
        int key = key(characters);

        if (key != 0) {
            view.forward(key);
            return;
        }

        insert(characters);
    }

    /**
     * Commits {@code characters} without line breaks and tabs at the
     * selection, after any composition is finished. False when nothing is
     * left to insert.
     */
    private boolean insert(CharSequence characters) {
        CharSequence text = plain(characters);

        if (text.length() == 0) {
            return false;
        }

        beginBatchEdit();

        try {
            removeComposingSpans(view.mirror());
            super.commitText(text, 1);
        } finally {
            endBatchEdit();
        }

        return true;
    }

    /**
     * Copies the mirror's selection to the clipboard, and deletes it for a
     * cut. False when nothing is selected or the field is a password field,
     * whose text is never copied.
     */
    private boolean copy(boolean cut) {
        Editable text = view.mirror();
        int[] selection = selection(text);

        if (view.isSecure() || selection == null
                || selection[0] == selection[1]) {
            return false;
        }

        ClipboardManager clipboard = view.clipboard();

        if (clipboard == null) {
            return false;
        }

        clipboard.setPrimaryClip(ClipData.newPlainText(null,
                text.subSequence(selection[0], selection[1]).toString()));

        if (cut) {
            beginBatchEdit();

            try {
                text.delete(selection[0], selection[1]);
            } finally {
                endBatchEdit();
            }
        }

        return true;
    }

    /**
     * Inserts the clipboard's text, its items one after the other. False
     * when it holds no text. Never logged.
     */
    private boolean paste() {
        ClipboardManager clipboard = view.clipboard();
        ClipData clip = clipboard == null ? null : clipboard.getPrimaryClip();

        if (clip == null) {
            return false;
        }

        StringBuilder text = new StringBuilder();

        for (int index = 0; index < clip.getItemCount(); index++) {
            CharSequence item =
                    clip.getItemAt(index).coerceToText(view.getContext());

            if (item != null) {
                text.append(item);
            }
        }

        return insert(text);
    }

    /**
     * The key a text that is only a line break or a tab stands for, as on
     * iOS: Enter for {@code "\n"}, {@code "\r"} and {@code "\r\n"} (what
     * keyboards commit for Enter), Tab for {@code "\t"}; else 0.
     */
    private static int key(CharSequence text) {
        if (TextUtils.equals(text, "\n") || TextUtils.equals(text, "\r")
                || TextUtils.equals(text, "\r\n")) {
            return KeyEvent.KEYCODE_ENTER;
        }

        if (TextUtils.equals(text, "\t")) {
            return KeyEvent.KEYCODE_TAB;
        }

        return 0;
    }

    /**
     * The text without its line breaks and tabs, which the app never gets
     * inside longer text: what is committed is then in the field, and every
     * Backspace over it deletes there.
     */
    private static CharSequence plain(CharSequence text) {
        if (text == null) {
            return null;
        }

        int length = text.length();
        StringBuilder plain = null;

        for (int index = 0; index < length; index++) {
            char c = text.charAt(index);

            if (c == '\n' || c == '\r' || c == '\t') {
                if (plain == null) {
                    plain = new StringBuilder(length);
                    plain.append(text, 0, index);
                }
            } else if (plain != null) {
                plain.append(c);
            }
        }

        return plain == null ? text : plain.toString();
    }

    /**
     * Deletes the selection, or the code point before the cursor. False when
     * there is nothing to delete.
     */
    private boolean deleteBackward() {
        beginBatchEdit();

        try {
            Editable text = view.mirror();
            int[] selection = selection(text);

            if (selection == null) {
                return false;
            }

            int start = selection[0];
            int end = selection[1];

            if (start == end) {
                start = backward(text, start, 1);
            }

            if (start == end) {
                return false;
            }

            text.delete(start, end);
            return true;
        } finally {
            endBatchEdit();
        }
    }

    /**
     * Deletes the selection, or the code point after the cursor. False when
     * there is nothing to delete.
     */
    private boolean deleteForward() {
        beginBatchEdit();

        try {
            Editable text = view.mirror();
            int[] selection = selection(text);

            if (selection == null) {
                return false;
            }

            int start = selection[0];
            int end = selection[1];

            if (start == end) {
                end = forward(text, end, 1);
            }

            if (start == end) {
                return false;
            }

            text.delete(start, end);
            return true;
        } finally {
            endBatchEdit();
        }
    }

    /**
     * Deletes {@code beforeLength} UTF-16 units (or code points) before the
     * selection and the composing region, and {@code afterLength} after
     * them, as {@link BaseInputConnection} does.
     *
     * <p>A side the keyboard asks to delete that is empty, while nothing is
     * selected or composing, is the Backspace or Delete key instead: the
     * text there is the field's, before or after the mirror. Keyboards such
     * as AOSP's LatinIME and its forks delete that way even when their
     * cursor is at the start of the text, where others send the key.
     */
    private void deleteAround(int beforeLength, int afterLength,
            boolean codePoints, String what) {
        boolean keyBefore = false;
        boolean keyAfter = false;

        beginBatchEdit();

        try {
            Editable text = view.mirror();
            int[] around = around(text);

            if (around != null) {
                int start = around[0];
                int end = around[1];
                int before;
                int after;

                if (codePoints) {
                    before = backward(text, start, beforeLength);
                    after = forward(text, end, afterLength);
                } else {
                    before = (int) Math.max(0L,
                            (long) start - Math.max(0, beforeLength));
                    after = (int) Math.min((long) text.length(),
                            (long) end + Math.max(0, afterLength));
                }

                boolean bare = isBare(text);
                keyBefore = bare && beforeLength > 0 && start == 0;
                keyAfter = bare && afterLength > 0 && end == text.length();

                delete(text, before, start, end, after);
            }
        } catch (RuntimeException error) {
            Keyboard.warn(what, error);
        } finally {
            endBatchEdit();
        }

        // After the edit's report, so the keys come after its text.
        if (keyBefore) {
            view.forward(KeyEvent.KEYCODE_DEL);
        }

        if (keyAfter) {
            view.forward(KeyEvent.KEYCODE_FORWARD_DEL);
        }
    }

    /** Whether nothing is selected and nothing is composing. */
    private static boolean isBare(Editable text) {
        int[] selection = selection(text);
        int start = getComposingSpanStart(text);
        int end = getComposingSpanEnd(text);

        return selection != null && selection[0] == selection[1]
                && (start < 0 || end < 0 || start == end);
    }

    /**
     * The selection, in order and inside the text, or null when there is
     * none.
     */
    private static int[] selection(Editable text) {
        int start = Selection.getSelectionStart(text);
        int end = Selection.getSelectionEnd(text);

        if (start < 0 || end < 0) {
            return null;
        }

        int length = text.length();

        return new int[] {
            Math.min(Math.min(start, end), length),
            Math.min(Math.max(start, end), length),
        };
    }

    /**
     * The selection grown to cover the composing region, which the
     * surrounding text excludes, or null when there is no selection.
     */
    private static int[] around(Editable text) {
        int[] around = selection(text);

        if (around == null) {
            return null;
        }

        int start = getComposingSpanStart(text);
        int end = getComposingSpanEnd(text);

        if (start >= 0 && end >= 0) {
            around[0] = Math.min(around[0], Math.min(start, end));
            around[1] = Math.max(around[1], Math.max(start, end));
        }

        return around;
    }

    /** Deletes [before, start) and [end, after), with start <= end. */
    private static void delete(Editable text, int before, int start, int end,
            int after) {
        if (end < after) {
            text.delete(end, after);
        }

        if (before < start) {
            text.delete(before, start);
        }
    }

    /**
     * The index {@code count} code points before {@code index}, or 0. An
     * unpaired surrogate counts as one code point.
     */
    private static int backward(CharSequence text, int index, int count) {
        while (count > 0 && index > 0) {
            index--;

            if (index > 0 && Character.isLowSurrogate(text.charAt(index))
                    && Character.isHighSurrogate(text.charAt(index - 1))) {
                index--;
            }

            count--;
        }

        return index;
    }

    /**
     * The index {@code count} code points after {@code index}, or the
     * length. An unpaired surrogate counts as one code point.
     */
    private static int forward(CharSequence text, int index, int count) {
        int length = text.length();

        while (count > 0 && index < length) {
            index++;

            if (index < length && Character.isLowSurrogate(text.charAt(index))
                    && Character.isHighSurrogate(text.charAt(index - 1))) {
                index++;
            }

            count--;
        }

        return index;
    }
}
