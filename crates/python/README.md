# resonator (Python)

Python bindings for the resonator node: create a node in a directory,
serve it, admit peers, and speak SQL, SPARQL, and chat with them from a
notebook. A native extension (pyo3, abi3) wrapping the Rust node; every
call blocks and releases the GIL, so it coexists with Jupyter's event
loop.

```python
import resonator as rsntr

node = rsntr.Node("alice")          # inits the directory on first use
node.serve()                        # background serving until stop()
print(node.ticket())                # dialing ticket for a peer

node.add_peer("bob", "endpoint...") # ticket or 64-hex endpoint id
res = node.query("bob", "SELECT id, body FROM notes")
res.columns, res.rows               # plain lists; NULL -> None
res.to_dicts()
res.to_pandas()                     # pip install resonator[pandas]

node.local("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)")
node.load_turtle("@prefix ex: <http://example.org/> . ex:a ex:b ex:c .")
node.sparql("SELECT ?s ?p ?o WHERE { ?s ?p ?o }").to_pandas()

node.chat_init()
node.chat_send("bob", "hello from a notebook")
node.chat_log("bob", limit=10)

node.stop()
```

A refused request raises `resonator.Denied`; protocol or engine errors
raise `resonator.QueryError` carrying the envelope error code. See
`docs/notebooks/quickstart.ipynb` in the repository for a two-node
walkthrough.

Build from source with [maturin](https://maturin.rs): `maturin build
--release` in this directory.

This package, and resonator itself, are dual licensed MIT OR Apache-2.0,
at your option.
