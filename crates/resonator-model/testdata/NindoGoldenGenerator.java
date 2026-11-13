// Generates nindo_golden.txt from the original Apache Wave Java
// implementation, for the differential test of the conversion of
// non-invertible document operations (Nindo) to DocOps in src/doc/nindo.rs
// (src/model_golden_tests/nindo.rs).
//
// For each of a number of seeds, a document is grown from empty by random
// Nindos made by the reference RandomNindoGenerator (first structural ones,
// then annotation-only ones, then both mixed), each converted by the
// reference IndexedDocumentImpl.consumeAndReturnInvertible.
//
// To regenerate: as for ModelGoldenGenerator.java, with this class and
// output file crates/resonator-model/testdata/nindo_golden.txt.
//
// Output format: one record per line, tab-separated, first field a tag;
// characters outside printable ASCII and backslashes are written \\uXXXX.
//
//   R <seed>               start of a run, with an empty document
//   n <component>          a component of the next Nindo, in order:
//                            K <count>             skip
//                            C <text>              characters
//                            ES <tag> {<name> <value>}   element start
//                            EE                    element end
//                            DC <count>            delete characters
//                            DS / DE               delete element start / end
//                            RA {<name> <value>}   replace attributes
//                            UA {<name> =<value> | <name> -}   update attributes
//                            SA <key> =<value> | SA <key> -    start annotation
//                            EA <key>              end annotation
//   o <concise doc op>     the DocOp the Nindo was converted to, normalized
//   d <xml>                the document after it

import org.waveprotocol.wave.model.document.indexed.IndexedDocument;
import org.waveprotocol.wave.model.document.operation.Attributes;
import org.waveprotocol.wave.model.document.operation.DocOp;
import org.waveprotocol.wave.model.document.operation.Nindo;
import org.waveprotocol.wave.model.document.operation.automaton.DocumentSchema;
import org.waveprotocol.wave.model.document.operation.impl.DocInitializationBuilder;
import org.waveprotocol.wave.model.document.operation.impl.DocOpUtil;
import org.waveprotocol.wave.model.document.raw.impl.Element;
import org.waveprotocol.wave.model.document.raw.impl.Node;
import org.waveprotocol.wave.model.document.raw.impl.Text;
import org.waveprotocol.wave.model.document.util.DocProviders;
import org.waveprotocol.wave.model.schema.conversation.ConversationSchemas;
import org.waveprotocol.wave.model.testing.RandomDocOpGenerator.RandomProvider;
import org.waveprotocol.wave.model.testing.RandomNindoGenerator;
import org.waveprotocol.wave.model.testing.RandomDocOpGenerator.Parameters.AnnotationOption;
import org.waveprotocol.wave.model.testing.RandomDocOpGenerator.Parameters;
import org.waveprotocol.wave.model.testing.RandomProviderImpl;

import java.io.PrintStream;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;

public class NindoGoldenGenerator {
  static PrintStream out;

  static void line(Object... fields) {
    StringBuilder b = new StringBuilder();
    for (int i = 0; i < fields.length; i++) {
      if (i > 0) b.append('\t');
      String s = String.valueOf(fields[i]);
      for (int j = 0; j < s.length(); j++) {
        char c = s.charAt(j);
        if (c >= 0x20 && c < 0x7F && c != '\\') {
          b.append(c);
        } else {
          b.append(String.format("\\u%04X", (int) c));
        }
      }
    }
    out.println(b);
  }

  static void attributes(List<Object> fields, Map<String, String> attrs, boolean update) {
    for (Map.Entry<String, String> e : new TreeMap<String, String>(attrs).entrySet()) {
      fields.add(e.getKey());
      if (update) {
        fields.add(e.getValue() == null ? "-" : "=" + e.getValue());
      } else {
        fields.add(e.getValue());
      }
    }
  }

  static void print(Nindo nindo) {
    nindo.apply(new Nindo.NindoCursor() {
      void n(Object... fields) {
        List<Object> all = new ArrayList<Object>();
        all.add("n");
        all.addAll(Arrays.asList(fields));
        line(all.toArray());
      }

      void a(String kind, Map<String, String> attrs, boolean update, Object... fields) {
        List<Object> all = new ArrayList<Object>();
        all.add(kind);
        all.addAll(Arrays.asList(fields));
        attributes(all, attrs, update);
        n(all.toArray());
      }

      @Override public void begin() {}
      @Override public void finish() {}
      @Override public void characters(String s) { n("C", s); }
      @Override public void elementStart(String type, Attributes attrs) {
        a("ES", attrs, false, type);
      }
      @Override public void elementEnd() { n("EE"); }
      @Override public void startAnnotation(String key, String value) {
        n("SA", key, value == null ? "-" : "=" + value);
      }
      @Override public void endAnnotation(String key) { n("EA", key); }
      @Override public void skip(int count) { n("K", count); }
      @Override public void deleteCharacters(int count) { n("DC", count); }
      @Override public void deleteElementStart() { n("DS"); }
      @Override public void deleteElementEnd() { n("DE"); }
      @Override public void replaceAttributes(Attributes attrs) { a("RA", attrs, false); }
      @Override public void updateAttributes(Map<String, String> update) {
        a("UA", update, true);
      }
    });
  }

  static Parameters withAnnotations(Parameters params) {
    params.setAnnotationOptions(Arrays.asList(new AnnotationOption[] {
        new AnnotationOption("a", Arrays.asList(new String[] {"1", "2", "3", "4"})),
        new AnnotationOption("b", Arrays.asList(new String[] {"1", "2", "3", "4"})),
        new AnnotationOption("x", Arrays.asList(new String[] {"1", "2", "3", "4"})),
    }));
    return params;
  }

  public static void main(String[] args) throws Exception {
    out = new PrintStream(System.out, false, "UTF-8");
    Parameters struct = new Parameters().setMaxOpeningComponents(15);
    struct.setAnnotationOptions(Collections.<AnnotationOption>emptyList());
    Parameters anno = withAnnotations(new Parameters().setMaxOpeningComponents(31));
    anno.setElementTypes(Collections.<String>emptyList());
    Parameters general = withAnnotations(new Parameters().setMaxOpeningComponents(31));
    DocumentSchema schema = ConversationSchemas.BLIP_SCHEMA_CONSTRAINTS;

    for (int seed = 1; seed <= 40; seed++) {
      line("R", seed);
      RandomProvider random = RandomProviderImpl.ofSeed(seed);
      IndexedDocument<Node, Element, Text> doc = DocProviders.POJO.build(
          new DocInitializationBuilder().build(), DocumentSchema.NO_SCHEMA_CONSTRAINTS);
      IndexedDocument<Node, Element, Text> copy = DocProviders.POJO.build(
          new DocInitializationBuilder().build(), DocumentSchema.NO_SCHEMA_CONSTRAINTS);
      for (int i = 0; i < 30; i++) {
        Parameters params = i < 8 ? struct : i < 16 ? anno : general;
        Nindo nindo = RandomNindoGenerator.generate(random, params, schema, copy);
        print(nindo);
        DocOp op = doc.consumeAndReturnInvertible(nindo);
        copy.consume(op);
        line("o", DocOpUtil.toConciseString(DocOpUtil.normalize(op)));
        line("d", DocOpUtil.toXmlString(doc.asOperation()));
      }
    }
    out.flush();
  }
}
