# Marqi sample document

## Heading levels

### Third level

#### Fourth level

##### Fifth level

###### Sixth level

Setext heading
==============

Setext subheading
-----------------

## Paragraphs and inline styles

A paragraph with **bold**, *italic*, ***bold italic***, ~~strikethrough~~ and
`inline code`.

This paragraph spans two source lines, so the soft break
collapses into a space when rendered.

A hard line break ends this line\
and this line follows it.

Escapes: \*not italic\*, \`not code\`, and \# not a heading.

Emphasis around CJK text: **中文加粗** and *日本語斜体*.

Keywords get colors in Marqi: TODO write more, NOTE the shade, WARNING hot
surface, ERROR not good, FIXME later, IMPORTANT read this, HACK temporary.

## Lists

- first item
- second item with **emphasis**
  - nested item
- [x] a completed task
- [ ] an open task

1. ordered one
2. ordered two

5) ordered from five with a paren delimiter
6) and the item after it

- a loose item

- another loose item

  with a second paragraph inside the item

-
- the bullet above is empty

## Block quotes

> A quote that spans
> two source lines.

> An outer quote
> > with a nested quote inside,
>
> and a second paragraph after it.

## Code

```rust
fn main() {
    println!("Hello, Marqi!");
}
```

```
a fenced block with no language,
including one long line that should hard-wrap on a narrow terminal window.
```

    an indented code block
    on two source lines

## Tables

| Left | Center | Right |
| :--- | :----: | ----: |
| a    | b      | c     |
| long cell | x | 42 |

## Links, images, and footnotes

An inline [link](https://example.com) and a reference [link][docs].

An autolink https://example.org, a www one www.example.net, and an
angle-bracket one <https://example.net>.

An image ![demo animation](../assets/demo.gif) inline.

A footnote[^note] reference; its definition sits at the bottom of the file.

---

The rule above is a thematic break.

## Unicode stress line

ASCII, then CJK 中文字符, then emoji 👍🏽 and a ZWJ family 👨‍👩‍👧‍👦, then more text.

The end.

[docs]: https://example.com/docs
[^note]: A footnote definition that lives at the bottom of the file.
