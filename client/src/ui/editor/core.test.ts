import { describe, expect, it } from "vitest";
import { DocOpBuilder } from "../../model/docop";
import { coreFixture } from "./testing";

// Locations: <body> is 0, the first <line> 1..2, its content starts at 3.

describe("typing", () => {
  it("inserts text at the caret and moves the caret after it", () => {
    const f = coreFixture();
    f.select(3);
    expect(f.core.insertText("h")).toBe(true);
    f.core.insertText("i");
    expect(f.body()).toBe("<line/>hi");
    expect(f.core.selection()).toEqual({ anchor: 5, focus: 5 });
  });

  it("replaces a selection", () => {
    const f = coreFixture("<line/>hello world");
    f.select(3, 8);
    f.core.insertText("bye");
    expect(f.body()).toBe("<line/>bye world");
    expect(f.core.selection()).toEqual({ anchor: 6, focus: 6 });
  });

  it("drops characters a blip cannot hold", () => {
    const f = coreFixture();
    f.select(3);
    f.core.insertText("a\u0000b\u202Ec\tdx\uD83D");
    expect(f.body()).toBe("<line/>abc dx\uFFFD");
  });

  it("splits text with line breaks into lines", () => {
    const f = coreFixture("<line/>ab");
    f.select(4);
    f.core.insertText("x\ny\r\nz");
    expect(f.body()).toBe("<line/>ax<line/>y<line/>zb");
    expect(f.core.selection()?.focus).toBe(11);
  });

  it("inherits styles from the left, not from the right", () => {
    const f = coreFixture("<line/>abcd");
    f.doc.edit((d) => d.setAnnotation(3, 5, "style/fontWeight", "bold"));
    f.select(5);
    f.core.insertText("X");
    expect(f.body()).toBe('<line/><?a "style/fontWeight"="bold"?>abX<?a "style/fontWeight"?>cd');
    f.select(3);
    f.core.insertText("Y");
    // At the start of a line: the style of the text to the right.
    expect(f.body()).toBe('<line/><?a "style/fontWeight"="bold"?>YabX<?a "style/fontWeight"?>cd');
  });

  it("does not extend a link at its end or its start", () => {
    const f = coreFixture("<line/>ab link cd");
    f.doc.edit((d) => d.setAnnotation(6, 10, "link/manual", "http://x.example/"));
    f.select(10);
    f.core.insertText("!");
    f.select(6);
    f.core.insertText("?");
    expect(f.body()).toBe('<line/>ab ?<?a "link/manual"="http://x.example/"?>link<?a "link/manual"?>! cd');
    // Inside the link it does extend.
    f.select(9);
    f.core.insertText("_");
    expect(f.body()).toContain("li_nk");
    expect(f.body()).toContain('<?a "link/manual"="http://x.example/"?>li_nk<?a "link/manual"?>');
  });

  it("applies a style toggled at the caret to the next typed text only once", () => {
    const f = coreFixture("<line/>ab");
    f.select(5);
    f.core.exec({ type: "toggleStyle", style: "bold" });
    expect(f.core.state(true, true).styles.bold).toBe(true);
    expect(f.doc.history.length).toBe(0);
    f.core.insertText("c");
    f.core.insertText("d");
    expect(f.body()).toBe('<line/>ab<?a "style/fontWeight"="bold"?>cd<?a "style/fontWeight"?>');
    f.core.exec({ type: "toggleStyle", style: "bold" });
    expect(f.core.state(true, true).styles.bold).toBe(false);
    f.core.insertText("e");
    expect(f.body()).toBe('<line/>ab<?a "style/fontWeight"="bold"?>cd<?a "style/fontWeight"?>e');
  });

  it("forgets the pending style when the caret moves", () => {
    const f = coreFixture("<line/>ab");
    f.select(5);
    f.core.exec({ type: "toggleStyle", style: "italic" });
    f.select(4);
    f.core.insertText("c");
    expect(f.body()).toBe("<line/>acb");
  });

  it("gives replacement text the style of the first replaced character", () => {
    const f = coreFixture("<line/>abcd");
    f.doc.edit((d) => d.setAnnotation(4, 6, "style/fontStyle", "italic"));
    f.select(4, 7);
    f.core.insertText("X");
    expect(f.body()).toBe('<line/>a<?a "style/fontStyle"="italic"?>X<?a "style/fontStyle"?>');
  });
});

describe("Enter", () => {
  it("splits a paragraph", () => {
    const f = coreFixture("<line/>abcd");
    f.select(5);
    f.core.enter();
    expect(f.body()).toBe("<line/>ab<line/>cd");
    expect(f.core.selection()).toEqual({ anchor: 7, focus: 7 });
  });

  it("at the end of a heading gives a normal paragraph", () => {
    const f = coreFixture('<line t="h1"/>Title');
    f.select(8);
    f.core.enter();
    expect(f.body()).toBe('<line t="h1"/>Title<line/>');
  });

  it("in the middle of a heading leaves the second half a paragraph, keeping alignment and indent", () => {
    const f = coreFixture('<line a="c" i="2" t="h2"/>Title');
    f.select(5);
    f.core.enter();
    expect(f.body()).toBe('<line a="c" i="2" t="h2"/>Ti<line a="c" i="2"/>tle');
  });

  it("continues a list", () => {
    const f = coreFixture('<line listyle="decimal" t="li"/>one');
    f.select(6);
    f.core.enter();
    expect(f.body()).toBe('<line listyle="decimal" t="li"/>one<line listyle="decimal" t="li"/>');
  });

  it("on an empty list item ends the list", () => {
    const f = coreFixture('<line t="li"/>one<line t="li"/>');
    f.select(8);
    f.core.enter();
    expect(f.body()).toBe('<line t="li"/>one<line/>');
    expect(f.core.selection()?.focus).toBe(8);
  });

  it("on an empty indented list item outdents first", () => {
    const f = coreFixture('<line i="1" t="li"/>');
    f.select(3);
    f.core.enter();
    expect(f.body()).toBe('<line t="li"/>');
    f.core.enter();
    expect(f.body()).toBe("<line/>");
    f.core.enter();
    expect(f.body()).toBe("<line/><line/>");
  });

  it("replaces the selection", () => {
    const f = coreFixture("<line/>abcd<line/>ef");
    f.select(4, 10);
    f.core.enter();
    expect(f.body()).toBe("<line/>a<line/>f");
  });
});

describe("Backspace and Delete", () => {
  it("deletes the character before the caret", () => {
    const f = coreFixture("<line/>abc");
    f.select(5);
    f.core.deleteBackward();
    expect(f.body()).toBe("<line/>ac");
    expect(f.core.selection()?.focus).toBe(4);
  });

  it("deletes a whole emoji, flag and combining sequence", () => {
    const f = coreFixture("<line/>a\u{1F600}e\u0301\u{1F1E9}\u{1F1EA}");
    f.select(12);
    f.core.deleteBackward();
    expect(f.body()).toBe("<line/>a\u{1F600}e\u0301");
    f.core.deleteBackward();
    expect(f.body()).toBe("<line/>a\u{1F600}");
    f.core.deleteBackward();
    expect(f.body()).toBe("<line/>a");
    f.select(3);
    f.core.insertText("\u{1F468}\u200D\u{1F469}\u200D\u{1F467}");
    f.select(3);
    f.core.deleteForward();
    expect(f.body()).toBe("<line/>a");
  });

  it("at the start of a paragraph joins it to the previous line", () => {
    const f = coreFixture("<line/>ab<line/>cd");
    f.select(7);
    f.core.deleteBackward();
    expect(f.body()).toBe("<line/>abcd");
    expect(f.core.selection()?.focus).toBe(5);
  });

  it("never deletes the first line", () => {
    const f = coreFixture("<line/>ab");
    f.select(3);
    expect(f.core.deleteBackward()).toBe(false);
    expect(f.body()).toBe("<line/>ab");
    f.select(0, 5);
    f.core.deleteBackward();
    expect(f.body()).toBe("<line/>");
  });

  it("at the start of an indented list item outdents, then unlists, then joins", () => {
    const f = coreFixture('<line/>a<line i="1" t="li"/>b');
    f.select(6);
    f.core.deleteBackward();
    expect(f.body()).toBe('<line/>a<line t="li"/>b');
    f.core.deleteBackward();
    expect(f.body()).toBe("<line/>a<line/>b");
    f.core.deleteBackward();
    expect(f.body()).toBe("<line/>ab");
  });

  it("joining into an empty line keeps the joined line's attributes", () => {
    const f = coreFixture('<line/><line t="h1"/>Title');
    f.select(5);
    f.core.deleteBackward();
    expect(f.body()).toBe('<line t="h1"/>Title');
    expect(f.core.selection()?.focus).toBe(3);
  });

  it("Delete at the end of a line joins the next line", () => {
    const f = coreFixture('<line/>ab<line t="h1"/>cd');
    f.select(5);
    f.core.deleteForward();
    expect(f.body()).toBe("<line/>abcd");
    f.select(7);
    expect(f.core.deleteForward()).toBe(false);
  });

  it("deletes words and to the line boundary", () => {
    const f = coreFixture("<line/>one two  three");
    f.select(17);
    f.core.deleteBackward("word");
    expect(f.body()).toBe("<line/>one two  ");
    f.core.deleteBackward("word");
    expect(f.body()).toBe("<line/>one ");
    f.select(3);
    f.core.deleteForward("word");
    expect(f.body()).toBe("<line/> ");
    f.core.insertText("abc def");
    f.select(6);
    f.core.deleteBackward("line");
    expect(f.body()).toBe("<line/> def ");
    f.core.deleteForward("line");
    expect(f.body()).toBe("<line/>");
  });

  it("does nothing next to an inline reply anchor, but a ranged deletion removes it", () => {
    const f = coreFixture('<line/>ab<reply id="t1"/>cd');
    f.select(7);
    expect(f.core.deleteBackward()).toBe(false);
    f.select(5);
    expect(f.core.deleteForward()).toBe(false);
    expect(f.body()).toBe('<line/>ab<reply id="t1"/>cd');
    f.select(4, 8);
    f.core.deleteBackward();
    expect(f.body()).toBe("<line/>ad");
  });

  it("deletes an image as a unit", () => {
    const f = coreFixture('<line/>a<image attachment="x"><caption>cap</caption></image>b');
    f.select(11);
    f.core.deleteBackward();
    expect(f.body()).toBe("<line/>ab");
  });

  it("deletes a selection across lines, joining them", () => {
    const f = coreFixture('<line/>abc<line t="li"/>def<line/>ghi');
    f.select(5, 14);
    f.core.deleteSelection();
    expect(f.body()).toBe("<line/>abhi");
  });

  it("keeps a selection out of atoms and line elements", () => {
    const f = coreFixture('<line/>a<image attachment="x"><caption>cap</caption></image><line/>b');
    f.select(6, 14);
    expect(f.core.range()).toEqual({ start: 11, end: 14 });
    f.select(2, 6);
    expect(f.core.range()).toEqual({ start: 3, end: 4 });
  });
});

describe("Tab", () => {
  it("indents and outdents every selected line", () => {
    const f = coreFixture('<line t="li"/>a<line/>b<line/>c');
    f.select(3, 7);
    f.core.indent(1);
    expect(f.body()).toBe('<line i="1" t="li"/>a<line i="1"/>b<line/>c');
    f.core.indent(-1);
    f.core.indent(-1);
    expect(f.body()).toBe('<line t="li"/>a<line/>b<line/>c');
  });

  it("indents by Tab only in lists and indented lines", () => {
    const f = coreFixture('<line t="li"/>a<line/>b<line i="1"/>c');
    f.select(3, 3);
    expect(f.core.tabIndents(1)).toBe(true);
    expect(f.core.tabIndents(-1)).toBe(false);
    f.select(6, 6);
    expect(f.core.tabIndents(1)).toBe(false);
    expect(f.core.tabIndents(-1)).toBe(false);
    f.select(9, 9);
    expect(f.core.tabIndents(1)).toBe(true);
    expect(f.core.tabIndents(-1)).toBe(true);
    f.select(3, 6);
    expect(f.core.tabIndents(1)).toBe(false);
  });
});

describe("commands", () => {
  it("toggles a style over a range", () => {
    const f = coreFixture("<line/>abcd");
    f.select(4, 6);
    f.core.exec({ type: "toggleStyle", style: "bold" });
    expect(f.body()).toBe('<line/>a<?a "style/fontWeight"="bold"?>bc<?a "style/fontWeight"?>d');
    expect(f.core.state(true, true).styles.bold).toBe(true);
    expect(f.core.range()).toEqual({ start: 4, end: 6 });
    f.select(3, 6);
    expect(f.core.state(true, true).styles.bold).toBe(false);
    f.core.exec({ type: "toggleStyle", style: "bold" });
    f.core.exec({ type: "toggleStyle", style: "bold" });
    expect(f.body()).toBe('<line/>abc<?a "style/fontWeight"?>d'.replace('<?a "style/fontWeight"?>', ""));
  });

  it("underline and strikethrough replace each other, as do super- and subscript", () => {
    const f = coreFixture("<line/>ab");
    f.select(3, 5);
    f.core.exec({ type: "toggleStyle", style: "underline" });
    f.core.exec({ type: "toggleStyle", style: "strikethrough" });
    f.core.exec({ type: "toggleStyle", style: "superscript" });
    f.core.exec({ type: "toggleStyle", style: "subscript" });
    expect(f.body()).toBe(
      '<line/><?a "style/textDecoration"="line-through" "style/verticalAlign"="sub"?>ab<?a "style/textDecoration" "style/verticalAlign"?>',
    );
    const s = f.core.state(true, true);
    expect(s.styles).toMatchObject({ underline: false, strikethrough: true, subscript: true, superscript: false });
  });

  it("sets font, size and colours", () => {
    const f = coreFixture("<line/>ab");
    f.select(3, 5);
    f.core.exec({ type: "setFontSize", value: "2em" });
    f.core.exec({ type: "setFontFamily", value: "georgia,serif" });
    f.core.exec({ type: "setColor", value: "rgb(255, 0, 0)" });
    f.core.exec({ type: "setBackgroundColor", value: "rgb(255, 255, 0)" });
    const s = f.core.state(true, true);
    expect(s).toMatchObject({
      fontSize: "2em",
      fontFamily: "georgia,serif",
      color: "rgb(255, 0, 0)",
      backgroundColor: "rgb(255, 255, 0)",
      hasRange: true,
    });
    f.core.exec({ type: "clearFormatting" });
    expect(f.body()).toBe("<line/>ab");
  });

  it("sets headings and lists, which exclude each other", () => {
    const f = coreFixture("<line/>a<line/>b");
    f.select(3, 7);
    f.core.exec({ type: "setHeading", level: 2 });
    expect(f.body()).toBe('<line t="h2"/>a<line t="h2"/>b');
    expect(f.core.state(true, true).heading).toBe(2);
    f.core.exec({ type: "toggleList", list: "numbered" });
    expect(f.body()).toBe('<line listyle="decimal" t="li"/>a<line listyle="decimal" t="li"/>b');
    expect(f.core.state(true, true)).toMatchObject({ list: "numbered", heading: 0 });
    f.core.exec({ type: "toggleList", list: "bullet" });
    expect(f.body()).toBe('<line t="li"/>a<line t="li"/>b');
    f.core.exec({ type: "toggleList", list: "bullet" });
    expect(f.body()).toBe("<line/>a<line/>b");
    f.core.exec({ type: "toggleList", list: "bullet" });
    f.core.exec({ type: "setHeading", level: 0 });
    expect(f.body()).toBe("<line/>a<line/>b");
  });

  it("aligns lines", () => {
    const f = coreFixture("<line/>a");
    f.select(3);
    f.core.exec({ type: "setAlignment", value: "center" });
    expect(f.body()).toBe('<line a="c"/>a');
    expect(f.core.state(true, true).alignment).toBe("center");
    f.core.exec({ type: "setAlignment", value: "left" });
    expect(f.body()).toBe("<line/>a");
  });

  it("creates and removes links", () => {
    const f = coreFixture("<line/>see here now");
    f.select(7, 11);
    f.core.exec({ type: "setLink", href: "example.com/a" });
    expect(f.body()).toBe('<line/>see <?a "link/manual"="http://example.com/a"?>here<?a "link/manual"?> now');
    f.select(9);
    expect(f.core.state(true, true).link).toBe("http://example.com/a");
    f.core.exec({ type: "setLink", href: "https://other.example/" });
    expect(f.body()).toContain('"link/manual"="https://other.example/"?>here<');
    f.core.exec({ type: "setLink", href: "javascript:alert(1)" });
    expect(f.body()).toContain('"link/manual"="https://other.example/"?>here<');
    f.core.exec({ type: "removeLink" });
    expect(f.body()).toBe("<line/>see here now");
  });

  it("inserts an attachment at the caret", () => {
    const f = coreFixture("<line/>ab");
    f.select(4);
    f.core.exec({ type: "insertAttachment", attachmentId: "att1", caption: "cat.png" });
    expect(f.body()).toBe('<line/>a<image attachment="att1"><caption>cat.png</caption></image>b');
    expect(f.core.selection()?.focus).toBe(15);
  });
});

describe("URL detection", () => {
  it("links a URL as it is typed and follows its changes", () => {
    const f = coreFixture();
    f.select(3);
    for (const ch of "go www.a.io") {
      f.core.insertText(ch);
    }
    expect(f.body()).toBe('<line/>go <?a "link/auto"="http://www.a.io"?>www.a.io<?a "link/auto"?>');
    f.core.insertText(" x");
    expect(f.body()).toBe('<line/>go <?a "link/auto"="http://www.a.io"?>www.a.io<?a "link/auto"?> x');
    f.select(7);
    f.core.insertText(" ");
    expect(f.body()).toBe("<line/>go w ww.a.io x");
  });

  it("leaves trailing punctuation out and handles several URLs", () => {
    const f = coreFixture();
    f.select(3);
    f.core.insertText("(see https://a.example/x?y=1). Or http://b.example/(z), ok");
    expect(f.body()).toBe(
      '<line/>(see <?a "link/auto"="https://a.example/x\\qy=1"?>https://a.example/x?y=1<?a "link/auto"?>). Or ' +
        '<?a "link/auto"="http://b.example/(z)"?>http://b.example/(z)<?a "link/auto"?>, ok',
    );
  });

  it("does not put back a link the user removed", () => {
    const f = coreFixture();
    f.select(3);
    f.core.insertText("http://a.example");
    f.select(5);
    f.core.exec({ type: "removeLink" });
    expect(f.body()).toBe("<line/>http://a.example");
  });
});

describe("paste", () => {
  it("inserts rich blocks with their own annotations and line types", () => {
    const f = coreFixture("<line/>ab");
    f.doc.edit((d) => d.setAnnotation(3, 5, "style/fontWeight", "bold"));
    f.select(4);
    f.core.insertBlocks(
      [
        { runs: [{ text: "x", annotations: {} }] },
        { type: "li", decimal: true, runs: [{ text: "y", annotations: { "style/fontStyle": "italic" } }] },
      ],
      true,
    );
    expect(f.body()).toBe(
      '<line/><?a "style/fontWeight"="bold"?>a<?a "style/fontWeight"?>x<line listyle="decimal" t="li"/>' +
        '<?a "style/fontStyle"="italic"?>y<?a "style/fontStyle" "style/fontWeight"="bold"?>b<?a "style/fontWeight"?>',
    );
    expect(f.core.selection()?.focus).toBe(8);
  });

  it("gives an empty line the type of the first pasted block", () => {
    const f = coreFixture();
    f.select(3);
    f.core.insertBlocks([{ type: "h1", runs: [{ text: "T", annotations: {} }] }, { runs: [{ text: "p", annotations: {} }] }], true);
    expect(f.body()).toBe('<line t="h1"/>T<line/>p');
  });
});

describe("undo", () => {
  it("undoes typing as one step and restores the selection", () => {
    const f = coreFixture("<line/>ab");
    f.select(5);
    f.core.insertText("c");
    f.core.insertText("d");
    expect(f.core.canUndo()).toBe(true);
    f.core.undo();
    expect(f.body()).toBe("<line/>ab");
    expect(f.core.selection()).toEqual({ anchor: 5, focus: 5 });
    expect(f.core.canUndo()).toBe(false);
    f.core.redo();
    expect(f.body()).toBe("<line/>abcd");
    expect(f.core.selection()?.focus).toBe(7);
  });

  it("splits typing into steps at words, pauses, caret moves and other actions", () => {
    const f = coreFixture();
    f.select(3);
    for (const ch of "ab cd") {
      f.core.insertText(ch);
    }
    f.time += 5000;
    f.core.insertText("e");
    f.core.enter();
    f.core.insertText("f");
    f.core.undo();
    expect(f.body()).toBe("<line/>ab cde<line/>");
    f.core.undo();
    expect(f.body()).toBe("<line/>ab cde");
    f.core.undo();
    expect(f.body()).toBe("<line/>ab cd");
    f.core.undo();
    expect(f.body()).toBe("<line/>ab ");
    f.core.undo();
    expect(f.body()).toBe("<line/>");
    expect(f.core.undo()).toBe(false);
    f.core.redo();
    f.core.redo();
    expect(f.body()).toBe("<line/>ab cd");
    f.core.insertText("!");
    expect(f.core.canRedo()).toBe(false);
  });

  it("undoes only this editor's operations, transformed over remote ones", () => {
    const f = coreFixture("<line/>abc");
    f.select(6);
    f.core.insertText("X");
    // Somebody else types at the start and after our text.
    f.doc.applyRemote(new DocOpBuilder().retain(3).characters("R").retain(5).build());
    f.doc.applyRemote(new DocOpBuilder().retain(8).characters("S").retain(1).build());
    expect(f.body()).toBe("<line/>RabcXS");
    f.core.undo();
    expect(f.body()).toBe("<line/>RabcS");
    expect(f.core.selection()?.focus).toBe(7);
    f.doc.applyRemote(new DocOpBuilder().retain(3).deleteCharacters("R").retain(5).build());
    f.core.redo();
    expect(f.body()).toBe("<line/>abcXS");
  });

  it("does not undo local edits made around the editor", () => {
    const f = coreFixture("<line/>abc");
    f.select(6);
    f.core.insertText("d");
    f.doc.edit((d) => d.createElement(4, "reply", [["id", "t9"]]));
    f.core.undo();
    expect(f.body()).toBe('<line/>a<reply id="t9"/>bc');
    expect(f.core.undo()).toBe(false);
  });

  it("undoes formatting and structure", () => {
    const f = coreFixture("<line/>ab<line/>cd");
    f.select(3, 9);
    f.core.exec({ type: "toggleStyle", style: "bold" });
    f.core.exec({ type: "toggleList", list: "bullet" });
    f.core.deleteSelection();
    f.core.undo();
    f.core.undo();
    f.core.undo();
    expect(f.body()).toBe("<line/>ab<line/>cd");
    expect(f.core.range()).toEqual({ start: 3, end: 9 });
  });
});

describe("selection across operations", () => {
  it("a caret stays before remote text inserted at it and after its own", () => {
    const f = coreFixture("<line/>ab");
    f.select(4);
    f.doc.applyRemote(new DocOpBuilder().retain(4).characters("R").retain(2).build());
    expect(f.core.selection()?.focus).toBe(4);
    f.doc.applyRemote(new DocOpBuilder().retain(3).characters("Q").retain(4).build());
    expect(f.core.selection()?.focus).toBe(5);
    f.doc.applyRemote(new DocOpBuilder().retain(3).deleteCharacters("Qa").retain(3).build());
    expect(f.core.selection()?.focus).toBe(3);
  });

  it("a range keeps its direction and excludes text inserted at its ends", () => {
    const f = coreFixture("<line/>abcd");
    f.select(6, 4);
    f.doc.applyRemote(new DocOpBuilder().retain(4).characters("R").retain(2).characters("S").retain(2).build());
    expect(f.core.selection()).toEqual({ anchor: 7, focus: 5 });
    f.doc.applyRemote(new DocOpBuilder().retain(3).deleteCharacters("aRbcSd").retain(1).build());
    expect(f.core.selection()).toEqual({ anchor: 3, focus: 3 });
  });
});

describe("mentions", () => {
  it("finds the @query typed before the caret", () => {
    const f = coreFixture("<line/>hi @ann");
    f.select(10);
    expect(f.core.mentionQuery()).toEqual({ start: 6, end: 10, query: "ann" });
    f.select(7);
    expect(f.core.mentionQuery()).toEqual({ start: 6, end: 7, query: "" });
    // In the middle of a word (an e-mail address) it is none.
    const g = coreFixture("<line/>bob@example.com");
    g.select(18);
    expect(g.core.mentionQuery()).toBeNull();
    // A full address of another server, a name with a space: one query.
    const h = coreFixture("<line/>@bob@other.org @Ann Lee");
    h.select(17);
    expect(h.core.mentionQuery()?.query).toBe("bob@other.org");
    h.select(26);
    expect(h.core.mentionQuery()).toEqual({ start: 18, end: 26, query: "Ann Lee" });
    // Not for a range, nor in the editor state while not editable.
    h.select(18, 20);
    expect(h.core.mentionQuery()).toBeNull();
    h.select(26);
    expect(h.core.state(false, false).mention).toBeNull();
    expect(h.core.state(true, true).mention?.query).toBe("Ann Lee");
  });

  it("replaces the query with an annotated name and a space", () => {
    const f = coreFixture("<line/>hi @an");
    f.select(9);
    f.core.exec({ type: "insertMention", start: 6, end: 9, text: "@Ann", address: "ann@example.com" });
    expect(f.body()).toBe('<line/>hi <?a "mention/user"="ann@example.com"?>@Ann<?a "mention/user"?> ');
    expect(f.core.selection()).toEqual({ anchor: 11, focus: 11 });
    // Typed text after it is not part of it, nor is a new query found in it.
    f.core.insertText("x");
    expect(f.body()).toBe('<line/>hi <?a "mention/user"="ann@example.com"?>@Ann<?a "mention/user"?> x');
    f.select(10);
    expect(f.core.mentionQuery()).toBeNull();
    // Undone in one step.
    f.core.undo();
    f.core.undo();
    expect(f.body()).toBe("<line/>hi @an");
  });

  it("does not add a second space before one that is there", () => {
    const f = coreFixture("<line/>@a b");
    f.select(5);
    f.core.exec({ type: "insertMention", start: 3, end: 5, text: "@Al", address: "al@example.com" });
    expect(f.body()).toBe('<line/><?a "mention/user"="al@example.com"?>@Al<?a "mention/user"?> b');
    expect(f.core.selection()?.focus).toBe(7);
  });
});
