// Generates supplement_golden.txt from the original Apache Wave Java
// implementation, for the differential tests of the adt, supplement and
// schema modules (src/supplement/golden_tests.rs).
//
// Each scenario drives the reference classes (the DocumentBased* data types,
// WaveletBasedSupplement, SupplementImpl and SupplementedWaveImpl over
// OpBasedWavelets; DocOpValidator with the reference schemas) through a
// scripted sequence of steps. The Rust test runs the same script against the
// port and expects the same records.
//
// To regenerate (from the workspace root, with a JDK, a protobuf-java jar and
// a Guava jar; compile in a scratch directory, never in the reference tree):
//
//   W=reference/apache-wave/wave/src; T=$(mktemp -d)
//   mkdir -p $T/javax/annotation
//   echo 'package javax.annotation; public @interface Nullable {}' \
//       > $T/javax/annotation/Nullable.java
//   javac -nowarn -cp $PROTOBUF_JAR:$GUAVA_JAR -d $T/out \
//       -sourcepath $T:$W/main/java:$W/test/java \
//       crates/resonator-model/testdata/SupplementGoldenGenerator.java
//   java -ea -cp $T/out:$PROTOBUF_JAR:$GUAVA_JAR SupplementGoldenGenerator \
//       > crates/resonator-model/testdata/supplement_golden.txt
//
// Output format: one record per line, tab-separated, first field a tag;
// characters outside printable ASCII and backslashes are written \\uXXXX.
//
//   N <scenario>                 start of a scenario
//   S <label>                    start of a step
//   O <wavelet id> <op>          an operation the step emitted, in order per
//                                wavelet, wavelets ordered by id;
//                                <op> is "add <address>", "remove <address>",
//                                "noop" or "doc <doc id> <concise doc op>",
//                                the doc op normalized
//   U <count>                    instead of the O records, for a step whose
//                                label starts with "~": the number of
//                                operations it emitted. Such a step deletes
//                                several elements in an order that depends on
//                                Java hash codes.
//   D <wavelet id> <doc id> <lmv> <xml>
//                                after the step, every non-empty document of
//                                every wavelet, ordered by wavelet and doc id,
//                                with its last-modified version
//   V <wavelet id> <doc id> <result> <first violation>
//                                after the step, for every non-empty document
//                                that has a schema: the result of validating
//                                its content against the schema
//   P <wavelet id> <version> <participants>
//                                after the step, every wavelet
//   E                            the step threw
//   Q <text>                     an answer to a query made by the step
//
// Every operation is applied with a version increment of zero and followed
// by a version update of one, as OpBasedWaveletFactory does in the
// reference tests.
//
// Supplement scenarios named "<name>/live" keep one WaveletBasedSupplement
// for the whole scenario; those named "<name>/fresh" create a new one in
// every step, as the port in effect does. Both must give the same records.

import org.waveprotocol.wave.model.adt.docbased.DocumentBasedBasicMap;
import org.waveprotocol.wave.model.adt.docbased.DocumentBasedBasicSet;
import org.waveprotocol.wave.model.adt.docbased.DocumentBasedBasicValue;
import org.waveprotocol.wave.model.adt.docbased.DocumentBasedBoolean;
import org.waveprotocol.wave.model.adt.docbased.DocumentBasedElementList;
import org.waveprotocol.wave.model.adt.docbased.DocumentBasedMonotonicMap;
import org.waveprotocol.wave.model.adt.docbased.DocumentBasedMonotonicValue;
import org.waveprotocol.wave.model.adt.docbased.DocumentBasedSingleton;
import org.waveprotocol.wave.model.adt.docbased.DocumentBasedStructuredValue;
import org.waveprotocol.wave.model.adt.docbased.Factory;
import org.waveprotocol.wave.model.adt.docbased.Initializer;
import org.waveprotocol.wave.model.conversation.Blips;
import org.waveprotocol.wave.model.conversation.ConversationBlip;
import org.waveprotocol.wave.model.conversation.ConversationThread;
import org.waveprotocol.wave.model.conversation.WaveBasedConversationView;
import org.waveprotocol.wave.model.conversation.WaveletBasedConversation;
import org.waveprotocol.wave.model.document.Doc;
import org.waveprotocol.wave.model.document.ObservableDocument;
import org.waveprotocol.wave.model.document.operation.DocInitialization;
import org.waveprotocol.wave.model.document.operation.DocOp;
import org.waveprotocol.wave.model.document.operation.automaton.DocOpAutomaton.ValidationResult;
import org.waveprotocol.wave.model.document.operation.automaton.DocOpAutomaton.ViolationCollector;
import org.waveprotocol.wave.model.document.operation.automaton.DocumentSchema;
import org.waveprotocol.wave.model.document.operation.impl.DocOpUtil;
import org.waveprotocol.wave.model.document.operation.impl.DocOpValidator;
import org.waveprotocol.wave.model.document.util.DefaultDocEventRouter;
import org.waveprotocol.wave.model.document.util.DocEventRouter;
import org.waveprotocol.wave.model.document.util.DocumentEventRouter;
import org.waveprotocol.wave.model.document.util.Point;
import org.waveprotocol.wave.model.document.util.XmlStringBuilder;
import org.waveprotocol.wave.model.id.IdGenerator;
import org.waveprotocol.wave.model.id.IdGeneratorImpl;
import org.waveprotocol.wave.model.id.IdUtil;
import org.waveprotocol.wave.model.id.WaveId;
import org.waveprotocol.wave.model.id.WaveletId;
import org.waveprotocol.wave.model.operation.OperationException;
import org.waveprotocol.wave.model.operation.SilentOperationSink;
import org.waveprotocol.wave.model.operation.wave.AddParticipant;
import org.waveprotocol.wave.model.operation.wave.BlipContentOperation;
import org.waveprotocol.wave.model.operation.wave.NoOp;
import org.waveprotocol.wave.model.operation.wave.RemoveParticipant;
import org.waveprotocol.wave.model.operation.wave.WaveletBlipOperation;
import org.waveprotocol.wave.model.operation.wave.WaveletOperation;
import org.waveprotocol.wave.model.operation.wave.WaveletOperationContext;
import org.waveprotocol.wave.model.schema.SchemaCollection;
import org.waveprotocol.wave.model.schema.account.AccountSchemas;
import org.waveprotocol.wave.model.schema.conversation.ConversationSchemas;
import org.waveprotocol.wave.model.schema.supplement.UserDataSchemas;
import org.waveprotocol.wave.model.supplement.PrimitiveSupplement;
import org.waveprotocol.wave.model.supplement.PrimitiveSupplementImpl;
import org.waveprotocol.wave.model.supplement.SimpleWantedEvaluation;
import org.waveprotocol.wave.model.supplement.SupplementImpl;
import org.waveprotocol.wave.model.supplement.SupplementWaveView;
import org.waveprotocol.wave.model.supplement.SupplementedWave;
import org.waveprotocol.wave.model.supplement.SupplementedWaveImpl;
import org.waveprotocol.wave.model.supplement.SupplementedWaveImpl.DefaultFollow;
import org.waveprotocol.wave.model.supplement.ThreadState;
import org.waveprotocol.wave.model.supplement.WantedEvaluation;
import org.waveprotocol.wave.model.supplement.WantedEvaluationSet;
import org.waveprotocol.wave.model.supplement.WaveletBasedSupplement;
import org.waveprotocol.wave.model.util.ReadableStringMap;
import org.waveprotocol.wave.model.util.Serializer;
import org.waveprotocol.wave.model.version.HashedVersion;
import org.waveprotocol.wave.model.version.HashedVersionSerializer;
import org.waveprotocol.wave.model.wave.ParticipantId;
import org.waveprotocol.wave.model.wave.ParticipationHelper;
import org.waveprotocol.wave.model.wave.Wavelet;
import org.waveprotocol.wave.model.wave.data.ObservableWaveletData;
import org.waveprotocol.wave.model.wave.data.ReadableBlipData;
import org.waveprotocol.wave.model.wave.data.impl.EmptyWaveletSnapshot;
import org.waveprotocol.wave.model.wave.data.impl.ObservablePluggableMutableDocument;
import org.waveprotocol.wave.model.wave.data.impl.WaveletDataImpl;
import org.waveprotocol.wave.model.wave.opbased.OpBasedWavelet;
import org.waveprotocol.wave.model.wave.opbased.WaveViewImpl;

import java.io.PrintStream;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.TreeSet;

public class SupplementGoldenGenerator {
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

  static final long TIME = 1234567890123L;
  static final ParticipantId ALICE = new ParticipantId("alice@example.com");
  static final ParticipantId BOB = new ParticipantId("bob@example.com");
  static final WaveId WAVE = WaveId.of("example.com", "w+wave");
  static final String ROOT = "conv+root";
  static final String OTHER = "conv+other";
  static final String UDW = "user+alice@example.com";
  static final SchemaCollection SCHEMAS = new SchemaCollection();
  static {
    SCHEMAS.add(new AccountSchemas());
    SCHEMAS.add(new ConversationSchemas());
    SCHEMAS.add(new UserDataSchemas());
  }

  static WaveletId wid(String id) {
    return WaveletId.of("example.com", id);
  }

  interface Step {
    void run() throws Exception;
  }

  /** A wave: wavelets that capture their operations. */
  static class Wave implements WaveViewImpl.WaveletFactory<OpBasedWavelet> {
    final ParticipantId author;
    final TreeMap<String, OpBasedWavelet> wavelets = new TreeMap<String, OpBasedWavelet>();
    final List<String[]> captured = new ArrayList<String[]>();

    Wave(ParticipantId author) {
      this.author = author;
    }

    @Override
    public OpBasedWavelet create(WaveId waveId, final WaveletId waveletId, ParticipantId creator) {
      final ObservableWaveletData data = WaveletDataImpl.Factory.create(
              ObservablePluggableMutableDocument.createFactory(SchemaCollection.empty()))
          .create(new EmptyWaveletSnapshot(waveId, waveletId, creator,
              HashedVersion.unsigned(0), TIME));
      WaveletOperationContext.Factory contexts = new WaveletOperationContext.Factory() {
        @Override
        public WaveletOperationContext createContext() {
          return new WaveletOperationContext(author, TIME, 0L);
        }

        @Override
        public WaveletOperationContext createContext(ParticipantId creator) {
          return new WaveletOperationContext(creator, TIME, 0L);
        }
      };
      SilentOperationSink<WaveletOperation> output = new SilentOperationSink<WaveletOperation>() {
        @Override
        public void consume(WaveletOperation op) {
          try {
            op.createVersionUpdateOp(1, null).apply(data);
          } catch (OperationException e) {
            throw new RuntimeException(e);
          }
          captured.add(new String[] {waveletId.getId(), describe(op)});
        }
      };
      OpBasedWavelet wavelet = new OpBasedWavelet(waveId, data, contexts,
          ParticipationHelper.DEFAULT,
          SilentOperationSink.Executor.<WaveletOperation, ObservableWaveletData>build(data),
          output);
      wavelets.put(waveletId.getId(), wavelet);
      return wavelet;
    }

    OpBasedWavelet wavelet(String id) {
      OpBasedWavelet w = wavelets.get(id);
      return w != null ? w : create(WAVE, WaveletId.of("example.com", id), author);
    }

    ObservableDocument doc(String wavelet, String doc) {
      return wavelet(wavelet).getDocument(doc);
    }

    /** Appends content to a document. */
    void append(String wavelet, String docId, String xml) {
      ObservableDocument doc = doc(wavelet, docId);
      doc.insertXml(Point.<Doc.N>end(doc.getDocumentElement()),
          XmlStringBuilder.createFromXmlString(xml));
    }

    void step(String label, Step step) {
      line("S", label);
      boolean failed = false;
      try {
        step.run();
      } catch (Exception e) {
        failed = true;
      } catch (AssertionError e) {
        failed = true;
      }
      if (label.startsWith("~")) {
        line("U", captured.size());
      } else {
        // Stable: operations of one wavelet stay in emission order.
        Collections.sort(captured, (a, b) -> a[0].compareTo(b[0]));
        for (String[] op : captured) {
          line("O", op[0], op[1]);
        }
      }
      captured.clear();
      if (failed) {
        line("E");
      }
      state();
    }

    void state() {
      for (Map.Entry<String, OpBasedWavelet> e : wavelets.entrySet()) {
        ObservableWaveletData data = e.getValue().getWaveletData();
        for (String docId : new TreeSet<String>(data.getDocumentIds())) {
          ReadableBlipData doc = data.getDocument(docId);
          DocInitialization content = doc.getContent().asOperation();
          String xml = DocOpUtil.toXmlString(content);
          if (!xml.isEmpty()) {
            line("D", e.getKey(), docId, doc.getLastModifiedVersion(), xml);
            DocumentSchema schema = SCHEMAS.getSchemaForId(wid(e.getKey()), docId);
            if (schema != DocumentSchema.NO_SCHEMA_CONSTRAINTS) {
              ViolationCollector v = new ViolationCollector();
              ValidationResult result = DocOpValidator.validate(v, schema, content);
              line("V", e.getKey(), docId, result, String.valueOf(v.firstDescription()));
            }
          }
        }
        StringBuilder participants = new StringBuilder();
        for (ParticipantId p : data.getParticipants()) {
          participants.append(p.getAddress()).append(' ');
        }
        line("P", e.getKey(), data.getVersion(), participants.toString().trim());
      }
    }
  }

  static String describe(WaveletOperation op) {
    if (op instanceof AddParticipant) {
      return "add " + ((AddParticipant) op).getParticipantId().getAddress();
    } else if (op instanceof RemoveParticipant) {
      return "remove " + ((RemoveParticipant) op).getParticipantId().getAddress();
    } else if (op instanceof NoOp) {
      return "noop";
    } else if (op instanceof WaveletBlipOperation) {
      WaveletBlipOperation blipOp = (WaveletBlipOperation) op;
      DocOp content = ((BlipContentOperation) blipOp.getBlipOp()).getContentOp();
      // Normalized: how the original splits retains and deletions depends on
      // the history of its DOM (text nodes are not merged again).
      return "doc " + blipOp.getBlipId() + " "
          + DocOpUtil.toConciseString(DocOpUtil.normalize(content));
    }
    throw new IllegalArgumentException("unexpected op " + op);
  }

  static void q(Object... answers) {
    StringBuilder b = new StringBuilder();
    for (Object answer : answers) {
      if (b.length() > 0) b.append(' ');
      b.append(answer);
    }
    line("Q", b);
  }

  static Map<String, String> attrs(String... pairs) {
    Map<String, String> map = new LinkedHashMap<String, String>();
    for (int i = 0; i < pairs.length; i += 2) {
      map.put(pairs[i], pairs[i + 1]);
    }
    return map;
  }

  /**
   * The items in order. Nulls are left out: the original takes an element
   * without key or value attribute for a null key or value, which the port
   * does not report.
   */
  static <T extends Comparable<T>> String sorted(Iterable<T> items) {
    TreeSet<T> set = new TreeSet<T>();
    for (T item : items) {
      if (item != null) {
        set.add(item);
      }
    }
    return set.toString();
  }

  static String sortedIds(Iterable<WaveletId> ids) {
    TreeSet<String> set = new TreeSet<String>();
    for (WaveletId id : ids) {
      set.add(id.getId());
    }
    return set.toString();
  }

  // ---- Document-based data types ----

  enum Key { alpha, beta }

  static final Factory<Doc.E, String, String> BAR = new Factory<Doc.E, String, String>() {
    @Override
    public String adapt(DocumentEventRouter<? super Doc.E, Doc.E, ?> router, Doc.E element) {
      return String.valueOf(router.getDocument().getAttribute(element, "bar"));
    }

    @Override
    public Initializer createInitializer(String initialState) {
      return DocumentBasedBasicValue.createInitialiser(Serializer.STRING, "bar", initialState);
    }
  };

  static void adtScenarios() {
    line("N", "adt-basic-map");
    {
      final Wave wave = new Wave(ALICE);
      final ObservableDocument doc = wave.doc(ROOT, "d");
      final DocEventRouter router = DefaultDocEventRouter.create(doc);
      final DocumentBasedBasicMap<Doc.E, String, String> map = DocumentBasedBasicMap.create(
          router, doc.getDocumentElement(), Serializer.STRING, Serializer.STRING, "e", "k", "v");
      wave.step("put a", () -> q(map.put("a", "1")));
      wave.step("put b", () -> q(map.put("b", "2")));
      wave.step("put a same", () -> q(map.put("a", "1")));
      wave.step("put a other", () -> q(map.put("a", "3")));
      wave.step("read", () -> q(map.get("a"), map.get("b"), map.get("c"), sorted(map.keySet())));
      wave.step("remove b", () -> map.remove("b"));
      wave.step("remove missing", () -> map.remove("zz"));
      wave.step("foreign content", () -> wave.append(ROOT, "d",
          "<e k=\"a\" v=\"9\"></e><x></x><e k=\"c\" v=\"1\"></e><e v=\"1\"></e>"));
      wave.step("read again", () -> q(map.get("a"), map.get("c"), sorted(map.keySet())));
      wave.step("~put c cleans up", () -> q(map.put("c", "2")));
      wave.step("read last", () -> q(map.get("a"), map.get("c"), sorted(map.keySet())));
    }

    line("N", "adt-monotonic-map");
    {
      final Wave wave = new Wave(ALICE);
      final ObservableDocument doc = wave.doc(ROOT, "d");
      final DocEventRouter router = DefaultDocEventRouter.create(doc);
      final DocumentBasedMonotonicMap<Doc.E, String, Integer> map =
          DocumentBasedMonotonicMap.create(router, doc.getDocumentElement(), Serializer.STRING,
              Serializer.INTEGER, "e", "k", "v");
      wave.step("put a 5", () -> q(map.put("a", 5)));
      wave.step("put a 3", () -> q(map.put("a", 3)));
      wave.step("put a 5 again", () -> q(map.put("a", 5)));
      wave.step("put b 1", () -> q(map.put("b", 1)));
      wave.step("put a 7", () -> q(map.put("a", 7)));
      wave.step("read", () -> q(map.get("a"), map.get("b"), map.get("c"), sorted(map.keySet())));
      wave.step("remove a", () -> map.remove("a"));
      wave.step("put a 2", () -> q(map.put("a", 2)));
      wave.step("foreign content", () -> wave.append(ROOT, "d",
          "<e k=\"b\" v=\"4\"></e><e k=\"b\" v=\"4\"></e>"));
      wave.step("read again", () -> q(map.get("a"), map.get("b"), sorted(map.keySet())));
      wave.step("~put b 9 cleans up", () -> q(map.put("b", 9)));
    }

    line("N", "adt-set");
    {
      final Wave wave = new Wave(ALICE);
      final ObservableDocument doc = wave.doc(ROOT, "d");
      final DocEventRouter router = DefaultDocEventRouter.create(doc);
      final DocumentBasedBasicSet<Doc.E, Integer> set = DocumentBasedBasicSet.create(
          router, doc.getDocumentElement(), Serializer.INTEGER, "f", "i");
      wave.step("add 5", () -> set.add(5));
      wave.step("add 7", () -> set.add(7));
      wave.step("add 5 again", () -> set.add(5));
      wave.step("remove 5", () -> set.remove(5));
      wave.step("remove missing", () -> set.remove(6));
      wave.step("foreign content", () -> wave.append(ROOT, "d",
          "<f i=\"7\"></f><x></x><f i=\"2\"></f>"));
      wave.step("read", () -> q(set.contains(7), set.contains(2), set.contains(5),
          sorted(set.getValues())));
      wave.step("add 9 cleans up", () -> set.add(9));
      wave.step("clear", () -> set.clear());
      wave.step("clear empty", () -> set.clear());
    }

    line("N", "adt-values");
    {
      final Wave wave = new Wave(ALICE);
      final ObservableDocument doc = wave.doc(ROOT, "d");
      final DocEventRouter router = DefaultDocEventRouter.create(doc);
      final DocumentBasedMonotonicValue<Doc.E, Integer> value = DocumentBasedMonotonicValue.create(
          router, doc.getDocumentElement(), Serializer.INTEGER, "m", "v");
      final DocumentBasedBoolean<Doc.E> flag = DocumentBasedBoolean.create(
          router, doc.getDocumentElement(), "b", "on");
      wave.step("value 3", () -> value.set(3));
      wave.step("value 2", () -> value.set(2));
      wave.step("value 5", () -> value.set(5));
      wave.step("flag true", () -> flag.set(true));
      wave.step("flag true again", () -> flag.set(true));
      wave.step("flag false", () -> flag.set(false));
      wave.step("read", () -> q(value.get(), flag.get()));
      wave.step("flag null", () -> flag.set(null));
      wave.step("flag null again", () -> flag.set(null));
      wave.step("foreign content", () -> wave.append(ROOT, "d",
          "<b on=\"true\"></b><b on=\"false\"></b><b></b><m v=\"5\"></m>"));
      wave.step("read again", () -> q(value.get(), flag.get()));
      wave.step("flag false cleans up", () -> flag.set(false));
      wave.step("~value 6 cleans up", () -> value.set(6));
    }

    line("N", "adt-list");
    {
      final Wave wave = new Wave(ALICE);
      final ObservableDocument doc = wave.doc(ROOT, "d");
      final DocEventRouter router = DefaultDocEventRouter.create(doc);
      wave.step("container", () -> wave.append(ROOT, "d", "<x></x><c><y></y></c>"));
      final Doc.E container = doc.asElement(doc.getLastChild(doc.getDocumentElement()));
      final DocumentBasedElementList<Doc.E, String, String> list =
          DocumentBasedElementList.create(router, container, "foo", BAR);
      wave.step("add x", () -> list.add("x"));
      wave.step("add y", () -> list.add("y"));
      wave.step("add z at 0", () -> list.add(0, "z"));
      wave.step("add null at 1", () -> list.add(1, null));
      wave.step("add w at end", () -> list.add(4, "w"));
      wave.step("add beyond end", () -> list.add(6, "v"));
      wave.step("read", () -> q(list.size(), list.getValues(), list.get(2), list.indexOf("y")));
      wave.step("remove y", () -> q(list.remove("y")));
      wave.step("remove missing", () -> q(list.remove("nope")));
      wave.step("clear", () -> list.clear());
    }

    line("N", "adt-singleton");
    {
      final Wave wave = new Wave(ALICE);
      final ObservableDocument doc = wave.doc(ROOT, "d");
      final DocEventRouter router = DefaultDocEventRouter.create(doc);
      final DocumentBasedSingleton<String, String> single = DocumentBasedSingleton.create(
          router, doc.getDocumentElement(), "foo", BAR);
      wave.step("read empty", () -> q(single.hasValue(), single.get()));
      wave.step("set 1", () -> q(single.set("1")));
      wave.step("set 2", () -> q(single.set("2")));
      wave.step("foreign content", () -> wave.append(ROOT, "d",
          "<foo bar=\"7\"></foo><w><foo bar=\"8\"></foo></w>"));
      wave.step("read", () -> q(single.hasValue(), single.get()));
      wave.step("set 3 cleans up", () -> q(single.set("3")));
      wave.step("more foreign content", () -> wave.append(ROOT, "d",
          "<foo bar=\"9\"></foo>"));
      wave.step("clear", () -> single.clear());
      wave.step("clear empty", () -> single.clear());
    }

    line("N", "adt-structured");
    {
      final Wave wave = new Wave(ALICE);
      final ObservableDocument doc = wave.doc(ROOT, "d");
      final DocEventRouter router = DefaultDocEventRouter.create(doc);
      wave.step("element", () -> wave.append(ROOT, "d", "<s></s>"));
      final Doc.E element = doc.asElement(doc.getFirstChild(doc.getDocumentElement()));
      final DocumentBasedStructuredValue<Doc.E, Key, Integer> value =
          DocumentBasedStructuredValue.create(router, element, Serializer.INTEGER, Key.class);
      final DocumentBasedBasicValue<Doc.E, Integer> basic =
          DocumentBasedBasicValue.create(router, element, Serializer.INTEGER, "alpha");
      wave.step("set alpha", () -> value.set(Key.alpha, 5));
      wave.step("set alpha same", () -> value.set(Key.alpha, 5));
      wave.step("set both", () -> {
        Map<Key, Integer> values = new HashMap<Key, Integer>();
        values.put(Key.alpha, 6);
        values.put(Key.beta, 7);
        value.set(values);
      });
      wave.step("set both same", () -> {
        Map<Key, Integer> values = new HashMap<Key, Integer>();
        values.put(Key.alpha, 6);
        values.put(Key.beta, 7);
        value.set(values);
      });
      wave.step("read", () -> q(value.get(Key.alpha), value.get(Key.beta), basic.get()));
      wave.step("remove beta", () -> value.set(Key.beta, null));
      wave.step("basic set", () -> basic.set(9));
      wave.step("basic set same", () -> basic.set(9));
      wave.step("basic remove", () -> basic.set(null));
      wave.step("read again", () -> q(value.get(Key.alpha), value.get(Key.beta), basic.get()));
    }
  }

  // ---- Supplement ----

  /** The view of the conversational wavelets of a wave, in order of id. */
  static class SortedView implements SupplementWaveView {
    final Wave wave;
    final ParticipantId viewer;

    SortedView(Wave wave, ParticipantId viewer) {
      this.wave = wave;
      this.viewer = viewer;
    }

    @Override
    public Iterable<WaveletId> getWavelets() {
      List<WaveletId> ids = new ArrayList<WaveletId>();
      for (OpBasedWavelet w : wave.wavelets.values()) {
        if (IdUtil.isConversationalId(w.getId())
            && WaveletBasedConversation.waveletHasConversation(w)) {
          ids.add(w.getId());
        }
      }
      return ids;
    }

    @Override
    public long getVersion(WaveletId id) {
      OpBasedWavelet w = wave.wavelets.get(id.getId());
      return w != null ? w.getVersion() : PrimitiveSupplement.NO_VERSION;
    }

    @Override
    public HashedVersion getSignature(WaveletId id) {
      OpBasedWavelet w = wave.wavelets.get(id.getId());
      return w != null ? w.getHashedVersion() : HashedVersion.unsigned(0);
    }

    @Override
    public Map<String, Long> getBlipVersions(WaveletId id) {
      return new HashMap<String, Long>();
    }

    @Override
    public boolean isExplicitParticipant() {
      for (WaveletId id : getWavelets()) {
        if (wave.wavelets.get(id.getId()).getParticipantIds().contains(viewer)) {
          return true;
        }
      }
      return false;
    }
  }

  /** A wave with the supplement of its author. */
  static class Sup {
    final Wave wave = new Wave(ALICE);
    final boolean fresh;
    final boolean adapter;
    final DefaultFollow follow;
    final IdGenerator ids = new IdGeneratorImpl("example.com", new IdGeneratorImpl.Seed() {
      @Override
      public String get() {
        return "seed";
      }
    });
    WaveBasedConversationView conversations;
    WaveletBasedSupplement prim;
    SupplementImpl sup;
    SupplementedWave sw;

    /**
     * @param fresh whether to create a new supplement in every step
     * @param adapter whether SupplementedWaveImpl gets its facts from the
     *        conversation model (its WaveViewAdapter) rather than from a
     *        SortedView
     */
    Sup(boolean fresh, boolean adapter, DefaultFollow follow) {
      this.fresh = fresh;
      this.adapter = adapter;
      this.follow = follow;
    }

    /** The conversation model; all conversational wavelets must exist. */
    WaveBasedConversationView conversations() {
      if (conversations == null) {
        WaveViewImpl<OpBasedWavelet> view = WaveViewImpl.create(wave, WAVE, ids, ALICE,
            WaveViewImpl.WaveletConfigurator.ERROR);
        for (OpBasedWavelet w : new ArrayList<OpBasedWavelet>(wave.wavelets.values())) {
          if (IdUtil.isConversationalId(w.getId())) {
            view.addWavelet(w);
          }
        }
        conversations = WaveBasedConversationView.create(view, ids);
      }
      return conversations;
    }

    void open() {
      if (prim == null || fresh) {
        prim = WaveletBasedSupplement.create(wave.wavelet(UDW));
        sup = new SupplementImpl(prim);
        sw = adapter
            ? SupplementedWaveImpl.create(prim, conversations(), ALICE, follow)
            : SupplementedWaveImpl.create(prim, new SortedView(wave, ALICE), follow);
      }
    }

    /** A step that uses the supplement. */
    void step(String label, final Step step) {
      wave.step(label, () -> {
        open();
        step.run();
      });
    }

    Wavelet w(String id) {
      return wave.wavelet(id);
    }

    ConversationBlip blip(String wavelet, String blip) {
      return conversations().getConversation(wid(wavelet)).getBlip(blip);
    }

    /** Builds a conversation with three blips, the third in a reply thread. */
    void conversation(String wavelet) {
      wave.append(wavelet, "conversation", "<conversation><blip id=\"b+1\"></blip>"
          + "<blip id=\"b+2\"><thread id=\"t+1\"><blip id=\"b+3\"></blip></thread></blip>"
          + "</conversation>");
      for (String blip : new String[] {"b+1", "b+2", "b+3"}) {
        wave.append(wavelet, blip, "<body><line></line>text</body>");
      }
    }

    void edit(String wavelet, String blip) {
      wave.doc(wavelet, blip).insertText(3, "x");
    }

    void unread(String wavelet) {
      q("unread", wavelet, sw.isUnread(blip(wavelet, "b+1")), sw.isUnread(blip(wavelet, "b+2")),
          sw.isUnread(blip(wavelet, "b+3")),
          "participants", sw.isParticipantsUnread(w(wavelet)),
          sw.haveParticipantsEverBeenRead(w(wavelet)),
          "tags", sw.isTagsUnread(w(wavelet)));
      WaveletId id = wid(wavelet);
      q("read versions", wavelet, prim.getLastReadBlipVersion(id, "b+1"),
          prim.getLastReadBlipVersion(id, "b+2"), prim.getLastReadBlipVersion(id, "b+3"),
          prim.getLastReadParticipantsVersion(id), prim.getLastReadTagsVersion(id),
          prim.getLastReadWaveletVersion(id), sorted(prim.getReadBlips(id)),
          sortedIds(prim.getReadWavelets()));
    }

    void inbox() {
      q("inbox", sw.isInbox(), "archived", sw.isArchived(), "mute", sw.isMute(),
          "followed", sw.isFollowed(), prim.getFollowed(), "trashed", sw.isTrashed(),
          "folders", sorted(sw.getFolders()));
      q("archive versions", prim.getArchiveWaveletVersion(wid(ROOT)),
          prim.getArchiveWaveletVersion(wid(OTHER)), sortedIds(prim.getArchiveWavelets()));
    }

    void seen() {
      q("seen", sw.hasBeenSeen(), hv(sw.getSeenVersion(wid(ROOT))),
          hv(sw.getSeenVersion(wid(OTHER))),
          sortedIds(prim.getSeenWavelets()),
          "notified", prim.getNotifiedVersion(wid(ROOT)), prim.getNotifiedVersion(wid(OTHER)),
          sortedIds(prim.getNotifiedWavelets()),
          "pending", sw.hasPendingNotification(), prim.getPendingNotification(),
          sup.hasPendingNotification(wid(ROOT)), sup.hasPendingNotification(wid(OTHER)));
    }
  }

  static String hv(HashedVersion version) {
    return HashedVersionSerializer.INSTANCE.toString(version);
  }

  static String mode(boolean fresh) {
    return fresh ? "/fresh" : "/live";
  }

  static void readScenario(boolean fresh) {
    line("N", "sup-read" + mode(fresh));
    final Sup s = new Sup(fresh, true, DefaultFollow.ALWAYS);
    s.wave.step("setup", () -> {
      s.conversation(ROOT);
      s.w(ROOT).addParticipant(ALICE);
      s.w(ROOT).addParticipant(BOB);
    });
    s.step("initial", () -> s.unread(ROOT));
    s.step("read b+1", () -> s.sw.markAsRead(s.blip(ROOT, "b+1")));
    s.step("after read b+1", () -> s.unread(ROOT));
    s.step("read b+1 again", () -> s.sw.markAsRead(s.blip(ROOT, "b+1")));
    s.step("edit b+1", () -> s.edit(ROOT, "b+1"));
    s.step("after edit b+1", () -> s.unread(ROOT));
    s.step("read b+1 after edit", () -> s.sw.markAsRead(s.blip(ROOT, "b+1")));
    s.step("read b+2", () -> s.sw.markAsRead(s.blip(ROOT, "b+2")));
    s.step("after read b+2", () -> s.unread(ROOT));
    s.step("read participants", () -> s.sw.markParticipantAsRead(s.w(ROOT)));
    s.step("read tags", () -> s.sw.markTagsAsRead(s.w(ROOT)));
    s.step("after read participants and tags", () -> s.unread(ROOT));
    s.step("read participants again", () -> s.sw.markParticipantAsRead(s.w(ROOT)));
    s.step("remove bob", () -> s.w(ROOT).removeParticipant(BOB));
    s.step("after remove bob", () -> s.unread(ROOT));
    s.step("read participants after change", () -> s.sw.markParticipantAsRead(s.w(ROOT)));
    s.step("read tags after change", () -> s.sw.markTagsAsRead(s.w(ROOT)));
    s.step("read wave", () -> s.sw.markAsRead());
    s.step("after read wave", () -> s.unread(ROOT));
    s.step("read wave again", () -> s.sw.markAsRead());
    s.step("read b+3 when wave read", () -> s.sw.markAsRead(s.blip(ROOT, "b+3")));
    s.step("edit b+3", () -> s.edit(ROOT, "b+3"));
    s.step("after edit b+3", () -> s.unread(ROOT));
    s.step("read b+3", () -> s.sw.markAsRead(s.blip(ROOT, "b+3")));
    s.step("read wave after edit", () -> s.sw.markAsRead());
    s.step("after read wave after edit", () -> s.unread(ROOT));
    s.step("mark blip read at old version", () -> s.sup.markBlipAsRead(wid(ROOT), "b+2", 3));
    s.step("mark blip read at future version",
        () -> s.sup.markBlipAsRead(wid(ROOT), "b+2", 1000));
    s.step("clear blip read state", () -> s.prim.clearBlipReadState(wid(ROOT), "b+1"));
    s.step("after clear blip read state", () -> s.unread(ROOT));
    s.step("unread", () -> s.sw.markAsUnread());
    s.step("after unread", () -> s.unread(ROOT));
    s.step("unread again", () -> s.sw.markAsUnread());
    s.step("read b+2 after unread", () -> s.sw.markAsRead(s.blip(ROOT, "b+2")));
    s.step("at the end", () -> s.unread(ROOT));
    s.step("copy", () -> {
      PrimitiveSupplementImpl copy = new PrimitiveSupplementImpl(s.prim);
      WaveletId id = wid(ROOT);
      q(copy.getLastReadBlipVersion(id, "b+1"), copy.getLastReadBlipVersion(id, "b+2"),
          copy.getLastReadParticipantsVersion(id), copy.getLastReadTagsVersion(id),
          copy.getLastReadWaveletVersion(id), sortedIds(copy.getReadWavelets()));
    });
  }

  static void inboxScenario(boolean fresh) {
    line("N", "sup-inbox" + mode(fresh));
    final Sup s = new Sup(fresh, true, DefaultFollow.ALWAYS);
    s.wave.step("setup", () -> {
      s.conversation(ROOT);
      s.w(ROOT).addParticipant(ALICE);
    });
    s.step("initial", () -> s.inbox());
    s.step("archive", () -> s.sw.archive());
    s.step("after archive", () -> s.inbox());
    s.step("archive again", () -> s.sw.archive());
    s.step("edit", () -> s.edit(ROOT, "b+1"));
    s.step("after edit", () -> s.inbox());
    s.step("archive after edit", () -> s.sw.archive());
    s.step("after archive after edit", () -> s.inbox());
    s.step("mute", () -> s.sw.mute());
    s.step("after mute", () -> s.inbox());
    s.step("mute again", () -> s.sw.mute());
    s.step("archive when muted", () -> s.sw.archive());
    s.step("follow", () -> s.sw.follow());
    s.step("after follow", () -> s.inbox());
    s.step("archive at same version after clear", () -> s.sw.archive());
    s.step("after archive at same version", () -> s.inbox());
    s.step("inbox", () -> s.sw.inbox());
    s.step("after inbox", () -> s.inbox());
    s.step("inbox again", () -> s.sw.inbox());
    s.step("trash", () -> s.sw.moveToFolder(SupplementedWaveImpl.TRASH_FOLDER));
    s.step("after trash", () -> s.inbox());
    s.step("move to folder 5", () -> s.sw.moveToFolder(5));
    s.step("after move to folder 5", () -> s.inbox());
    s.step("add folder 12", () -> s.prim.addFolder(12));
    s.step("add folder 12 again", () -> s.prim.addFolder(12));
    s.step("add folder 2", () -> s.prim.addFolder(2));
    s.step("after add folders", () -> {
      s.inbox();
      q(s.prim.isInFolder(12), s.prim.isInFolder(13));
    });
    s.step("remove folder 5", () -> s.prim.removeFolder(5));
    s.step("remove folder 5 again", () -> s.prim.removeFolder(5));
    s.step("move to all folder", () -> s.sw.moveToFolder(3));
    s.step("after move to all folder", () -> s.inbox());
    s.step("move to folder 6", () -> s.sw.moveToFolder(6));
    s.step("move to inbox folder", () -> s.sw.moveToFolder(1));
    s.step("after move to inbox folder", () -> s.inbox());
    s.step("unfollow", () -> s.sw.unfollow());
    s.step("clear follow", () -> s.prim.clearFollow());
    s.step("after clear follow", () -> s.inbox());
    s.step("clear follow again", () -> s.prim.clearFollow());
    s.step("clear archive", () -> s.sup.clearArchive());
    s.step("archive at lower version", () -> s.sup.archive(wid(ROOT), 2));
    s.step("after archive at lower version", () -> s.inbox());
  }

  static void followScenario(boolean fresh) {
    line("N", "sup-follow" + mode(fresh));
    final Sup s = new Sup(fresh, true, DefaultFollow.WHEN_PARTICIPANT);
    s.wave.step("setup", () -> s.conversation(ROOT));
    s.step("not a participant", () -> s.inbox());
    s.step("archive when not followed", () -> s.sw.archive());
    s.step("follow", () -> s.sw.follow());
    s.step("after follow", () -> s.inbox());
    s.step("clear follow", () -> s.prim.clearFollow());
    s.step("bob joins", () -> s.w(ROOT).addParticipant(BOB));
    s.step("after bob joins", () -> s.inbox());
    s.step("alice joins", () -> s.w(ROOT).addParticipant(ALICE));
    s.step("after alice joins", () -> s.inbox());
    s.step("unfollow", () -> s.sw.unfollow());
    s.step("after unfollow", () -> s.inbox());
  }

  static void multiScenario(boolean fresh) {
    line("N", "sup-multi" + mode(fresh));
    final Sup s = new Sup(fresh, false, DefaultFollow.ALWAYS);
    s.wave.step("setup", () -> {
      s.conversation(ROOT);
      s.conversation(OTHER);
      // Conversational by id, but without a manifest.
      s.wave.append("conv+empty", "b+1", "<body><line></line></body>");
      s.wave.append("other+data", "conversation", "<conversation></conversation>");
      s.w(OTHER).addParticipant(ALICE);
    });
    s.step("initial", () -> {
      s.unread(ROOT);
      s.unread(OTHER);
      s.inbox();
      s.seen();
    });
    s.step("read wave", () -> s.sw.markAsRead());
    s.step("after read wave", () -> {
      s.unread(ROOT);
      s.unread(OTHER);
    });
    s.step("edit other", () -> s.edit(OTHER, "b+2"));
    s.step("after edit other", () -> {
      s.unread(ROOT);
      s.unread(OTHER);
    });
    s.step("read wave after edit", () -> s.sw.markAsRead());
    s.step("archive", () -> s.sw.archive());
    s.step("after archive", () -> s.inbox());
    s.step("edit root", () -> s.edit(ROOT, "b+1"));
    s.step("after edit root", () -> s.inbox());
    s.step("archive after edit", () -> s.sw.archive());
    s.step("after archive after edit", () -> s.inbox());
    s.step("see at version zero", () -> s.sw.see());
    s.step("after see at version zero", () -> s.seen());
    s.step("sign", () -> {
      s.wave.wavelet(ROOT).getWaveletData().setHashedVersion(
          HashedVersion.of(40, new byte[] {1, 2, 3}));
      s.wave.wavelet(OTHER).getWaveletData().setHashedVersion(
          HashedVersion.of(41, new byte[] {(byte) 0xff, 0, 7, 9}));
    });
    s.step("see root", () -> s.sw.see(s.w(ROOT)));
    s.step("after see root", () -> s.seen());
    s.step("see", () -> s.sw.see());
    s.step("after see", () -> s.seen());
    s.step("see again", () -> s.sw.see());
    s.step("notify", () -> s.sw.markAsNotified());
    s.step("after notify", () -> s.seen());
    s.step("sign later", () -> s.wave.wavelet(OTHER).getWaveletData().setHashedVersion(
        HashedVersion.of(90, new byte[] {5})));
    s.step("see other", () -> s.sw.see(s.w(OTHER)));
    s.step("after see other", () -> s.seen());
    s.step("notified at 200", () -> s.prim.setNotifiedVersion(wid(OTHER), 200));
    s.step("notified at 100", () -> s.prim.setNotifiedVersion(wid(OTHER), 100));
    s.step("after notified", () -> s.seen());
    s.step("seen at lower version",
        () -> s.prim.setSeenVersion(wid(OTHER), HashedVersion.of(3, new byte[] {9})));
    s.step("clear seen other", () -> s.prim.clearSeenVersion(wid(OTHER)));
    s.step("after clear seen other", () -> s.seen());
    s.step("seen at lower version after clear",
        () -> s.prim.setSeenVersion(wid(OTHER), HashedVersion.of(3, new byte[] {9})));
    s.step("at the end", () -> s.seen());
    s.step("unread", () -> s.sw.markAsUnread());
  }

  static WantedEvaluation evaluation(String wavelet, boolean wanted, double certainty,
      long timestamp, String agent, boolean ignored) {
    return new SimpleWantedEvaluation(wid(wavelet), "bob@example.com", wanted, certainty,
        timestamp, agent, ignored, "a \"comment\"");
  }

  static String describe(WantedEvaluation e) {
    return e.getWaveletId().getId() + "/" + e.getAdderAddress() + "/" + e.isWanted() + "/"
        + e.getCertainty() + "/" + e.getTimestamp() + "/" + e.getAgentIdentity() + "/"
        + e.isIgnored() + "/" + e.getComment() + "/" + e.hashCode();
  }

  static void wanted(Sup s, String wavelet) {
    WantedEvaluationSet set = s.sw.getWantedEvaluationSet(s.w(wavelet));
    TreeSet<String> all = new TreeSet<String>();
    for (WantedEvaluation e : set.getEvaluations()) {
      all.add(describe(e));
    }
    q("wanted", wavelet, set.isWanted(), set.isIgnored(), describe(set.getMostCertain()),
        describe(set.getMostRecent()), all);
  }

  static String gadget(ReadableStringMap<String> state) {
    final TreeMap<String, String> map = new TreeMap<String, String>();
    state.each(new ReadableStringMap.ProcV<String>() {
      @Override
      public void apply(String key, String value) {
        map.put(key, value);
      }
    });
    return map.toString();
  }

  static void miscScenario(boolean fresh) {
    line("N", "sup-misc" + mode(fresh));
    final Sup s = new Sup(fresh, true, DefaultFollow.ALWAYS);
    s.wave.step("setup", () -> {
      s.conversation(ROOT);
      // An empty wavelet, to ask for the evaluations of.
      s.wave.wavelet(OTHER);
    });
    final WaveletId root = wid(ROOT);
    final WaveletId other = wid(OTHER);

    s.step("no thread state", () -> q(s.prim.getThreadState(root, "t+1"),
        sorted(s.prim.getStatefulThreads(root)), sortedIds(s.prim.getWaveletsWithThreadState())));
    s.step("remove thread state of unknown wavelet", () -> s.prim.setThreadState(root, "t+1", null));
    s.step("collapse t+1", () -> {
      ConversationThread thread = s.blip(ROOT, "b+2").getReplyThread("t+1");
      s.sw.setThreadState(thread, ThreadState.COLLAPSED);
      q(s.sw.getThreadState(thread));
    });
    s.step("collapse t+1 again", () -> s.prim.setThreadState(root, "t+1", ThreadState.COLLAPSED));
    s.step("expand t+1", () -> s.prim.setThreadState(root, "t+1", ThreadState.EXPANDED));
    s.step("collapse t+2", () -> s.prim.setThreadState(root, "t+2", ThreadState.COLLAPSED));
    s.step("collapse in other", () -> s.prim.setThreadState(other, "t+9", ThreadState.COLLAPSED));
    s.step("thread states", () -> q(s.prim.getThreadState(root, "t+1"),
        s.prim.getThreadState(root, "t+2"), s.prim.getThreadState(root, "t+3"),
        s.prim.getThreadState(other, "t+9"), sorted(s.prim.getStatefulThreads(root)),
        sorted(s.prim.getStatefulThreads(other)), sortedIds(s.prim.getWaveletsWithThreadState())));
    s.step("remove t+1", () -> s.prim.setThreadState(root, "t+1", null));
    s.step("remove t+1 again", () -> s.prim.setThreadState(root, "t+1", null));
    s.step("thread states after remove", () -> q(s.prim.getThreadState(root, "t+1"),
        sorted(s.prim.getStatefulThreads(root))));

    s.step("no gadget state", () -> q(gadget(s.sw.getGadgetState("g1")),
        s.sw.getGadgetStateValue("g1", "k")));
    s.step("gadget remove on unknown gadget", () -> s.sw.setGadgetState("g0", "k", null));
    s.step("gadget set", () -> s.sw.setGadgetState("g1", "k", "v"));
    s.step("gadget set same", () -> s.sw.setGadgetState("g1", "k", "v"));
    s.step("gadget set other key", () -> s.sw.setGadgetState("g1", "k2", "v <2> & \"more\""));
    s.step("gadget replace", () -> s.sw.setGadgetState("g1", "k", "w"));
    s.step("gadget in other", () -> s.sw.setGadgetState("g 2", "k", "x"));
    s.step("gadget states", () -> q(gadget(s.sw.getGadgetState("g0")),
        gadget(s.sw.getGadgetState("g1")), gadget(s.sw.getGadgetState("g 2")),
        s.sw.getGadgetStateValue("g1", "k2"), s.sw.getGadgetStateValue("g1", "zz")));
    s.step("gadget remove", () -> s.sw.setGadgetState("g1", "k", null));
    s.step("gadget states after remove", () -> q(gadget(s.sw.getGadgetState("g1"))));

    s.step("no evaluations", () -> wanted(s, ROOT));
    s.step("evaluation 1",
        () -> s.sw.addWantedEvaluation(evaluation(ROOT, true, 0.5, 1000, "agent1", false)));
    s.step("evaluation 2",
        () -> s.sw.addWantedEvaluation(evaluation(ROOT, false, 0.75, 900, "agent2", true)));
    s.step("evaluations", () -> wanted(s, ROOT));
    s.step("evaluation 3",
        () -> s.sw.addWantedEvaluation(evaluation(ROOT, true, 1.0E-4, 2000, "agent3", false)));
    s.step("evaluation 3 again",
        () -> s.sw.addWantedEvaluation(evaluation(ROOT, true, 1.0E-4, 2000, "agent3", false)));
    s.step("evaluation in other", () -> s.sw.addWantedEvaluation(
        evaluation(OTHER, false, 1.0E7, 1234567890123L, "agent1", false)));
    s.step("evaluation 4",
        () -> s.sw.addWantedEvaluation(evaluation(ROOT, true, 0.75, 900, "agent4", false)));
    s.step("more evaluations", () -> {
      wanted(s, ROOT);
      wanted(s, OTHER);
      q(s.prim.getWantedEvaluations().size());
    });
    s.step("odd certainties", () -> {
      for (double c : new double[] {0, 1, 100, 1234567.125, 12345678.5, 0.001, 0.00099, 1e21,
          1e-7, 0.1 + 0.2, 123456789012345680.0, 4.9e-324, 1.7976931348623157e308, 0.5e-3,
          9999999.999999998, 1.0E-5, 3.0E10}) {
        s.sw.addWantedEvaluation(evaluation(OTHER, true, c, 1, "odd", false));
      }
      s.sw.addWantedEvaluation(evaluation(OTHER, true, -2.5, -7, "odd", false));
    });
    s.step("all evaluations", () -> wanted(s, OTHER));
  }

  static void corruptScenario(boolean fresh) {
    line("N", "sup-corrupt" + mode(fresh));
    final Sup s = new Sup(fresh, true, DefaultFollow.ALWAYS);
    final WaveletId root = wid(ROOT);
    s.wave.step("setup", () -> {
      s.conversation(ROOT);
      s.w(ROOT).addParticipant(ALICE);
    });
    s.step("empty", () -> {
      s.unread(ROOT);
      s.inbox();
      s.seen();
    });
    s.step("corrupt read state", () -> s.wave.append(UDW, "m/read",
        "<wavelet i=\"example.com/conv+root\"><all v=\"5\"></all><participants v=\"3\">"
        + "</participants><blip i=\"b+1\" v=\"2\"></blip><all v=\"9\"></all>"
        + "<blip i=\"b+1\" v=\"7\"></blip><blip i=\"b+1\" v=\"4\"></blip>"
        + "<blip i=\"b+2\" v=\"1\"></blip><all v=\"7\"></all><tags v=\"1\"></tags>"
        + "<tags v=\"1\"></tags><junk></junk><blip i=\"b+9\" v=\"7\"></blip></wavelet>"
        + "<wavelet i=\"example.com/conv+root\"><all v=\"100\"></all>"
        + "<blip i=\"b+3\" v=\"50\"></blip></wavelet><wavelet></wavelet><other></other>"
        + "<wavelet i=\"example.com/conv+other\"><participants v=\"8\"></participants>"
        + "<participants v=\"2\"></participants></wavelet>"));
    s.step("corrupt read state read", () -> {
      s.unread(ROOT);
      q(s.prim.getLastReadParticipantsVersion(wid(OTHER)), s.prim.getLastReadBlipVersion(root, "b+9"));
    });
    s.step("read blip at lower version", () -> s.prim.setLastReadBlipVersion(root, "b+1", 6));
    s.step("~read blip cleans up", () -> s.prim.setLastReadBlipVersion(root, "b+2", 5));
    s.step("~read wavelet cleans up", () -> s.prim.setLastReadWaveletVersion(root, 12));
    s.step("~read tags cleans up", () -> s.prim.setLastReadTagsVersion(root, 2));
    s.step("~read participants of other cleans up",
        () -> s.prim.setLastReadParticipantsVersion(wid(OTHER), 9));
    s.step("after cleaning up", () -> s.unread(ROOT));

    s.step("corrupt muted", () -> s.wave.append(UDW, "m/muted",
        "<muted muted=\"false\"></muted><x></x><muted muted=\"true\"></muted><muted></muted>"));
    s.step("corrupt muted read", () -> s.inbox());
    s.step("unfollow when muted", () -> s.sw.unfollow());
    s.step("follow cleans up", () -> s.sw.follow());
    s.step("corrupt muted again", () -> s.wave.append(UDW, "m/muted",
        "<muted muted=\"TRUE\"></muted><muted muted=\"false\"></muted>"));
    s.step("clear follow cleans up", () -> s.prim.clearFollow());
    s.step("after clear follow", () -> s.inbox());

    s.step("corrupt archive", () -> {
      s.wave.append(UDW, "m/archiving", "<archive i=\"example.com/conv+root\" v=\"3\"></archive>"
          + "<archive i=\"example.com/conv+root\" v=\"8\"></archive>"
          + "<archive i=\"example.com/conv+root\" v=\"8\"></archive>");
      s.wave.append(UDW, "m/cleared", "<cleared cleared=\"true\"></cleared>"
          + "<cleared cleared=\"false\"></cleared><cleared cleared=\"true\"></cleared>");
    });
    s.step("corrupt archive read", () -> s.inbox());
    s.step("archive when cleared", () -> s.sup.archive(root, 5));
    s.step("after archive when cleared", () -> s.inbox());
    s.step("~archive cleans up", () -> s.sup.archive(root, 30));
    s.step("after archive", () -> s.inbox());

    s.step("corrupt seen", () -> s.wave.append(UDW, "m/seen",
        "<seen i=\"example.com/conv+root\" signature=\"5:AQI=\"></seen>"
        + "<seen i=\"example.com/conv+root\" signature=\"5:AQM=\"></seen>"
        + "<seen i=\"example.com/conv+other\" signature=\"7\"></seen>"
        + "<notified i=\"example.com/conv+root\" v=\"4\"></notified>"
        + "<notified i=\"example.com/conv+root\" v=\"2\"></notified>"
        + "<notification pending=\"true\"></notification>"
        + "<notification pending=\"true\"></notification>"));
    s.step("corrupt seen read", () -> s.seen());
    s.step("see cleans up", () -> s.sw.see());
    s.step("after see", () -> s.seen());

    s.step("corrupt presentation", () -> s.wave.append(UDW, "m/presentation",
        "<conversation i=\"example.com/conv+root\"><thread i=\"t+1\" state=\"COLLAPSED\">"
        + "</thread><thread i=\"t+2\" state=\"EXPANDED\"></thread>"
        + "<thread i=\"t+1\" state=\"EXPANDED\"></thread></conversation>"
        + "<conversation i=\"example.com/conv+root\"><thread i=\"t+3\" state=\"COLLAPSED\">"
        + "</thread></conversation>"));
    s.step("corrupt presentation read", () -> q(s.prim.getThreadState(root, "t+1"),
        s.prim.getThreadState(root, "t+2"), s.prim.getThreadState(root, "t+3"),
        sorted(s.prim.getStatefulThreads(root))));
    s.step("~thread state cleans up", () -> s.prim.setThreadState(root, "t+2", ThreadState.COLLAPSED));
    s.step("remove thread state", () -> s.prim.setThreadState(root, "t+1", null));

    s.step("corrupt gadgets", () -> s.wave.append(UDW, "m/gadgets",
        "<gadget i=\"g1\"><state name=\"a\" value=\"1\"></state><state name=\"b\" value=\"2\">"
        + "</state><state name=\"a\" value=\"3\"></state></gadget><gadget></gadget>"
        + "<gadget i=\"g1\"><state name=\"c\" value=\"4\"></state></gadget>"));
    s.step("corrupt gadgets read", () -> q(gadget(s.prim.getGadgetState("g1"))));
    s.step("gadget remove cleans up", () -> s.prim.setGadgetState("g1", "zz", null));
    s.step("after gadget remove", () -> q(gadget(s.prim.getGadgetState("g1"))));

    s.step("corrupt abuse", () -> s.wave.append(UDW, "m/abuse",
        "<wanted wavelet_id=\"example.com/conv+root\"></wanted><x></x>"
        + "<wanted wavelet_id=\"example.com/conv+root\" wanted=\"TRUE\" certainty=\" 1e1 \" "
        + "timestamp=\"+12\" ignored=\"yes\" adder=\"a\" agent=\"b\" comment=\"c\"></wanted>"
        + "<wanted wavelet_id=\"example.com/conv+root\"></wanted>"));
    s.step("corrupt abuse read", () -> wanted(s, ROOT));
  }

  /**
   * Clearing the read state deletes only the first element of a wavelet. A
   * supplement created afterwards takes the next element for the read state
   * of the wavelet; the supplement that did the clearing takes the wavelet
   * to have none (not shown here).
   */
  static void clearDuplicatesScenario() {
    line("N", "sup-clear-duplicates");
    final Sup s = new Sup(true, true, DefaultFollow.ALWAYS);
    s.wave.step("setup", () -> {
      s.conversation(ROOT);
      s.wave.append(UDW, "m/read",
          "<wavelet i=\"example.com/conv+root\"><all v=\"1\"></all></wavelet>"
          + "<wavelet i=\"example.com/conv+other\"><all v=\"2\"></all></wavelet>"
          + "<wavelet i=\"example.com/conv+root\"><all v=\"100\"></all>"
          + "<blip i=\"b+3\" v=\"50\"></blip></wavelet>");
    });
    s.step("before", () -> s.unread(ROOT));
    s.step("~unread", () -> s.sw.markAsUnread());
    s.step("after", () -> s.unread(ROOT));
    s.step("unread again", () -> s.sw.markAsUnread());
    s.step("at the end", () -> s.unread(ROOT));
  }

  static void foldersScenario() {
    line("N", "sup-legacy-folders");
    final Sup s = new Sup(true, true, DefaultFollow.ALWAYS);
    s.wave.step("setup", () -> {
      s.conversation(ROOT);
      s.wave.append(UDW, "m/folder", "<folder i=\"5\"></folder><folder i=\"1\"></folder>"
          + "<x></x><folder i=\"5\"></folder><folder i=\"3\"></folder><folder i=\"9\"></folder>");
    });
    s.step("create removes legacy folders", () -> s.inbox());
    s.step("add legacy folder", () -> {
      s.prim.addFolder(3);
      q(sorted(s.prim.getFolders()));
    });
    s.step("next create removes it", () -> s.inbox());
    s.step("add folders", () -> {
      s.prim.addFolder(2);
      s.prim.addFolder(15);
      s.prim.addFolder(0);
    });
    s.step("remove all folders", () -> s.prim.removeAllFolders());
    s.step("remove all folders again", () -> s.prim.removeAllFolders());
  }

  /** A user-data wavelet that has participants, written by a non-participant. */
  static void participantScenario() {
    line("N", "sup-udw-participants");
    final Sup s = new Sup(true, true, DefaultFollow.ALWAYS);
    s.wave.step("setup", () -> {
      s.conversation(ROOT);
      s.wave.wavelet(UDW).addParticipant(BOB);
    });
    s.step("follow", () -> s.sw.follow());
    s.step("unfollow", () -> s.sw.unfollow());
  }

  // ---- Schemas ----

  static void validate(String wavelet, String docId, String xml) {
    // The content of a document without schema that was given the XML.
    Wave scratch = new Wave(ALICE);
    scratch.doc("scratch+w", "d");
    if (!xml.isEmpty()) {
      scratch.append("scratch+w", "d", xml);
    }
    DocInitialization content =
        scratch.wavelet("scratch+w").getWaveletData().getDocument("d").getContent().asOperation();
    DocumentSchema schema = SCHEMAS.getSchemaForId(wid(wavelet), docId);
    ViolationCollector v = new ViolationCollector();
    ValidationResult result = DocOpValidator.validate(v, schema, content);
    line("Q", wavelet, docId, xml, result, String.valueOf(v.firstDescription()));
  }

  /**
   * One record for an attribute of an element type: whether it is
   * permitted, then each value followed by whether it is permitted.
   */
  static void attribute(String wavelet, String docId, String tag, String attr,
      String[]... values) {
    DocumentSchema schema = SCHEMAS.getSchemaForId(wid(wavelet), docId);
    List<Object> fields = new ArrayList<Object>();
    Collections.addAll(fields, "Q", wavelet, docId, tag, attr, schema.permitsAttribute(tag, attr));
    for (String[] group : values) {
      for (String value : group) {
        fields.add(value);
        // Where the original throws, the port answers false.
        try {
          fields.add(schema.permitsAttribute(tag, attr, value));
        } catch (RuntimeException e) {
          fields.add("false (exception)");
        }
      }
    }
    line(fields.toArray());
  }

  /**
   * One record for a parent element type: the text it permits, its
   * required initial children, then each child type followed by whether it
   * is permitted.
   */
  static void structure(String wavelet, String docId, String parent, String[] children) {
    DocumentSchema schema = SCHEMAS.getSchemaForId(wid(wavelet), docId);
    List<Object> fields = new ArrayList<Object>();
    Collections.addAll(fields, "Q", wavelet, docId, String.valueOf(parent),
        schema.permittedCharacters(parent), schema.getRequiredInitialChildren(parent));
    for (String child : children) {
      if (child != null) {
        fields.add(child);
        fields.add(schema.permitsChild(parent, child));
      }
    }
    line(fields.toArray());
  }

  static final String[] NUMBERS = {"0", "1", "-1", "+1", "12", "007", "2147483647", "2147483648",
      "-2147483648", "-2147483649", "9223372036854775807", "9223372036854775808", "", " 1", "1 ",
      "1.0", "1.", ".5", "1e5", "1E-5", "1e", "e5", "0x10", "0x1p3", "0x1.8p1", "0x.8p0", "0xp1",
      "NaN", "nan", "Infinity", "-Infinity", "+Infinity", "inf", "1f", "1d", "1D", "1F", "1L", "+",
      "-", "--1", "1_000", "1,5", " 1.5e3 ", "\t2\n", ".", "1e+5", "1e400", "1e-400", "true", "abc"};

  static void schemaScenarios() {
    line("N", "schema-lookup");
    String[][] ids = {
        {ROOT, "b+abc"}, {ROOT, "conversation"}, {ROOT, "roles"}, {ROOT, "tags"}, {ROOT, "m/read"},
        {ROOT, "abc*def"}, {ROOT, "g+abc"}, {ROOT, "attach+abc"}, {ROOT, "m/attachment/x"},
        {"conv+other", "b+abc"}, {"legacy", "b+abc"}, {"legacy", "conversation"},
        {UDW, "m/read"}, {UDW, "m/presentation"}, {UDW, "m/folder"}, {UDW, "m/archiving"},
        {UDW, "m/muted"}, {UDW, "m/cleared"}, {UDW, "m/abuse"}, {UDW, "m/seen"},
        {UDW, "m/gadgets"}, {UDW, "m/other"}, {UDW, "b+abc"}, {UDW, "conversation"},
        {"user+bob@example.com", "m/read"}, {"user", "m/read"}, {"userx+a", "m/read"},
        {"other+data", "b+abc"}, {"other+data", "m/read"},
    };
    for (String[] id : ids) {
      DocumentSchema schema = SCHEMAS.getSchemaForId(wid(id[0]), id[1]);
      line("Q", id[0], id[1], schema != DocumentSchema.NO_SCHEMA_CONSTRAINTS,
          schema.permitsChild(null, "body"), schema.permitsChild(null, "conversation"),
          schema.permitsChild(null, "assign"), schema.permitsChild(null, "wavelet"),
          schema.permitsChild(null, "anything"));
    }

    line("N", "schema-structure");
    String[] blipTags = {null, "body", "head", "timestamp", "lmt", "line", "textarea", "image",
        "caption", "reply", "gadget", "state", "pref", "category", "title", "thumbnail", "eqn",
        "label", "input", "button", "events", "click", "check", "radiogroup", "radio", "password",
        "profile", "profile-field", "mediasearch", "result", "customsearch", "trustreq", "trwave",
        "blacklist", "invitation", "invited", "settings", "bool-setting", "radio-setting",
        "text-setting", "listbox-setting", "html", "data", "experimental", "namevaluepair",
        "part", "translation", "stanza", "extension_installer", "ext-settings",
        "gadget-settings", "img", "quote", "unknown"};
    for (String parent : blipTags) {
      structure(ROOT, "b+abc", parent, blipTags);
    }
    String[] manifestTags = {null, "conversation", "blip", "thread", "peer", "unknown"};
    for (String parent : manifestTags) {
      structure(ROOT, "conversation", parent, manifestTags);
    }
    String[] udwDocs = {"m/read", "m/presentation", "m/folder", "m/archiving", "m/muted",
        "m/cleared", "m/abuse", "m/seen", "m/gadgets"};
    String[] udwTags = {null, "wavelet", "blip", "participants", "tags", "all", "conversation",
        "thread", "folder", "archive", "muted", "cleared", "wanted", "seen", "notified",
        "notification", "gadget", "state", "unknown"};
    for (String doc : udwDocs) {
      for (String parent : udwTags) {
        structure(UDW, doc, parent, udwTags);
      }
    }
    structure(ROOT, "roles", null, new String[] {"assign", "unknown"});
    structure(ROOT, "roles", "assign", new String[] {"assign", "unknown"});
    structure(UDW, "m/other", null, new String[] {"anything"});
    structure(UDW, "m/other", "anything", new String[] {"anything"});

    line("N", "schema-attributes");
    String[][] blipAttrs = {
        {"line", "t"}, {"line", "listyle"}, {"line", "a"}, {"line", "d"}, {"line", "i"},
        {"line", "x"}, {"image", "attachment"}, {"image", "style"}, {"reply", "id"},
        {"img", "width"}, {"img", "height"}, {"img", "src"}, {"img", "alt"}, {"lmt", "t"},
        {"invitation", "remaining"}, {"invitation", "title"}, {"eqn", "format"},
        {"gadget", "url"}, {"gadget", "name"}, {"state", "name"}, {"state", "value"},
        {"body", "x"}, {"unknown", "i"}, {"unknown", "width"}, {"click", "time"},
        {"check", "value"}, {"input", "value"}, {"radio", "group"}, {"label", "for"},
        {"text-setting", "live-value"}, {"bool-setting", "live-value"},
    };
    String[] values = {"h1", "h4", "h5", "li", "decimal", "l", "r", "c", "j", "full", "tex",
        "true", "false", "b+abc", ""};
    for (String[] attr : blipAttrs) {
      attribute(ROOT, "b+abc", attr[0], attr[1], values, NUMBERS);
    }
    String[][] manifestAttrs = {
        {"conversation", "anchorWavelet"}, {"conversation", "anchorManifestOffset"},
        {"conversation", "anchorVersion"}, {"conversation", "anchorBlip"},
        {"conversation", "anchorOffset"}, {"conversation", "sort"}, {"conversation", "id"},
        {"blip", "id"}, {"blip", "deleted"}, {"blip", "x"}, {"thread", "id"},
        {"thread", "inline"}, {"peer", "id"}, {"unknown", "id"}, {"unknown", "anchorBlip"},
    };
    String[] manifestValues = {"b+abc", "b+", "b", "abc", "abc*def", "g+abc", "m/abc*def",
        "attach+a*b", "spell+a*b", "true", "false", "TRUE", "", "0", "1", "-1", "12", "+3", "1.5",
        "example.com/conv+root", "example.com/conv+abc", "example.com/user+a@example.com",
        "example.com/legacy", "example.com/other+x", "example.com!conv+root", "conv+root",
        "example.com/conv+root/x", "example.com/", "/conv+root", "example.com/conv+a~b",
        "bad domain/conv+root"};
    for (String[] attr : manifestAttrs) {
      attribute(ROOT, "conversation", attr[0], attr[1], manifestValues);
    }
    String[][] udwAttrs = {
        {"wavelet", "i"}, {"blip", "i"}, {"blip", "v"}, {"participants", "v"}, {"tags", "v"},
        {"all", "v"}, {"all", "i"}, {"conversation", "i"}, {"thread", "i"}, {"thread", "state"},
        {"folder", "i"}, {"folder", "v"}, {"archive", "i"}, {"archive", "v"}, {"muted", "muted"},
        {"cleared", "cleared"}, {"wanted", "wavelet_id"}, {"wanted", "wanted"},
        {"wanted", "certainty"}, {"wanted", "timestamp"}, {"wanted", "agent"},
        {"wanted", "comment"}, {"wanted", "ignored"}, {"wanted", "adder"}, {"seen", "i"},
        {"seen", "signature"}, {"notified", "i"}, {"notified", "v"}, {"notification", "pending"},
        {"gadget", "i"}, {"gadget", "p"}, {"state", "name"}, {"state", "value"},
        {"unknown", "i"}, {"unknown", "v"}, {"unknown", "certainty"}, {"unknown", "timestamp"},
        {"unknown", "wavelet_id"}, {"unknown", "x"},
    };
    String[] udwValues = {"true", "false", "TRUE", "COLLAPSED", "EXPANDED", "example.com/conv+root",
        "b+abc"};
    for (String doc : udwDocs) {
      for (String[] attr : udwAttrs) {
        attribute(UDW, doc, attr[0], attr[1], udwValues, NUMBERS);
      }
    }
    String[] roles = {"FULL", "READ_ONLY", "full", "NONE", ""};
    attribute(ROOT, "roles", "assign", "role", roles);
    attribute(ROOT, "roles", "assign", "address", roles);
    attribute(ROOT, "roles", "assign", "x", roles);
    attribute(ROOT, "roles", "unknown", "role", roles);
    attribute(UDW, "m/other", "anything", "x", roles);

    line("N", "schema-documents");
    String[] blips = {
        "",
        "<body><line></line></body>",
        "<body><line></line>Hello <reply id=\"t+1\"></reply>world</body>",
        "<body>Hello</body>",
        "<body></body>",
        "<body><line t=\"h1\"></line>Title<line t=\"li\" i=\"2\" listyle=\"decimal\"></line>x</body>",
        "<body><line t=\"h7\"></line></body>",
        "<body><line i=\"0\"></line></body>",
        "<body><line x=\"1\"></line></body>",
        "<body><line></line><image attachment=\"a\"><caption>cap</caption></image></body>",
        "<body><line></line><image attachment=\"a\" style=\"wide\"></image></body>",
        "<body><line></line><line>text in line</line></body>",
        "<body><line></line><body><line></line></body></body>",
        "<head><timestamp><lmt t=\"12\"></lmt></timestamp></head><body><line></line></body>",
        "<head><timestamp><lmt t=\"-1\"></lmt></timestamp></head>",
        "<head>text</head>",
        "text",
        "<line></line>",
        "<body><line></line><textarea name=\"n\"><line></line>typed</textarea></body>",
        "<body><line></line><textarea name=\"n\">typed</textarea></body>",
        "<body><line></line><gadget url=\"u\"><state name=\"a\" value=\"b\"></state>"
            + "<pref name=\"a\" value=\"b\"></pref></gadget></body>",
        "<body><line></line><gadget url=\"u\">text</gadget></body>",
        "<body><line></line><img src=\"s\" width=\"10\" height=\"-1\"></img></body>",
        "<body><line></line><eqn format=\"tex\">x^2</eqn></body>",
        "<body><line></line>tab\there</body>",
        "<body><line></line>café 世界</body>",
        "<body><line></line><button name=\"b\"><caption>go</caption><events>"
            + "<click time=\"1\" clicker=\"a\"></click></events></button><check name=\"c\" "
            + "value=\"true\"></check><label for=\"c\">check</label><input name=\"i\">in</input>"
            + "</body>",
        "<body><line></line><quote><line></line>quoted</quote><quote>bad</quote></body>",
        "<body><line></line><unknown></unknown></body>",
        "<conversation></conversation>",
    };
    for (String xml : blips) {
      validate(ROOT, "b+abc", xml);
    }
    String[] manifests = {
        "",
        "<conversation></conversation>",
        "<conversation><blip id=\"b+1\"></blip><blip id=\"b+2\"><thread id=\"t+1\">"
            + "<blip id=\"b+3\"></blip></thread><thread id=\"t+2\" inline=\"true\"></thread>"
            + "</blip></conversation>",
        "<conversation anchorWavelet=\"example.com/conv+root\" anchorBlip=\"b+1\" "
            + "anchorVersion=\"3\" sort=\"x\"></conversation>",
        "<conversation anchorWavelet=\"example.com/user+a\"></conversation>",
        "<conversation anchorVersion=\"-3\"></conversation>",
        "<conversation><blip id=\"x\"></blip></conversation>",
        "<conversation><blip id=\"b+1\" deleted=\"yes\"></blip></conversation>",
        "<conversation><blip id=\"b+1\" deleted=\"true\"><peer id=\"p\"></peer></blip>"
            + "</conversation>",
        "<conversation><thread id=\"t\"></thread></conversation>",
        "<conversation>text</conversation>",
        "<conversation></conversation><conversation></conversation>",
        "<blip id=\"b+1\"></blip>",
        "<conversation><blip id=\"b+1\"><blip id=\"b+2\"></blip></blip></conversation>",
    };
    for (String xml : manifests) {
      validate(ROOT, "conversation", xml);
    }
    String[][] udw = {
        {"m/read", "<wavelet i=\"example.com/conv+root\"><all v=\"25\"></all>"
            + "<participants v=\"12\"></participants><blip i=\"b+1\" v=\"7\"></blip>"
            + "<tags v=\"0\"></tags></wavelet>"},
        {"m/read", "<wavelet i=\"x\"><all v=\"-1\"></all></wavelet>"},
        {"m/read", "<wavelet><blip v=\"x\"></blip></wavelet>"},
        {"m/read", "<wavelet><wavelet></wavelet></wavelet>"},
        {"m/read", "<blip i=\"b\" v=\"1\"></blip>"},
        {"m/read", "<wavelet>text</wavelet>"},
        {"m/presentation", "<conversation i=\"example.com/conv+root\">"
            + "<thread i=\"t\" state=\"COLLAPSED\"></thread></conversation>"},
        {"m/presentation", "<conversation><thread state=\"whatever\"></thread></conversation>"},
        {"m/presentation", "<thread></thread>"},
        {"m/folder", "<folder i=\"3\"></folder><folder i=\"-12\"></folder>"},
        {"m/folder", "<folder i=\"x\"></folder>"},
        {"m/folder", "<folder></folder>"},
        {"m/folder", "<folder v=\"3\"></folder>"},
        {"m/archiving", "<archive i=\"example.com/conv+root\" v=\"48\"></archive>"},
        {"m/archiving", "<archive i=\"anything\" v=\"-48\"></archive>"},
        {"m/muted", "<muted muted=\"true\"></muted><muted muted=\"false\"></muted>"},
        {"m/muted", "<muted muted=\"yes\"></muted>"},
        {"m/muted", "<muted></muted>"},
        {"m/muted", "<cleared cleared=\"true\"></cleared>"},
        {"m/cleared", "<cleared cleared=\"true\"></cleared>"},
        {"m/cleared", "<cleared cleared=\"1\"></cleared>"},
        {"m/seen", "<seen i=\"example.com/conv+root\" signature=\"12:aGFzaA==\"></seen>"
            + "<notified i=\"example.com/conv+root\" v=\"12\"></notified>"
            + "<notification pending=\"true\"></notification>"},
        {"m/seen", "<seen v=\"1\"></seen>"},
        {"m/seen", "<notified v=\"-1\"></notified><notification pending=\"maybe\"></notification>"},
        {"m/gadgets", "<gadget i=\"g\" p=\"x\"><state name=\"k\" value=\"v\"></state></gadget>"},
        {"m/gadgets", "<gadget><gadget></gadget></gadget>"},
        {"m/gadgets", "<state name=\"k\"></state>"},
        {"m/abuse", "<wanted wavelet_id=\"example.com/conv+root\" wanted=\"true\" "
            + "certainty=\"0.5\" timestamp=\"1234\" adder=\"a@b.c\" agent=\"x\" comment=\"y\" "
            + "ignored=\"false\"></wanted>"},
        {"m/abuse", "<wanted certainty=\"high\"></wanted>"},
        {"m/abuse", "<wanted timestamp=\"1.5\"></wanted>"},
        {"m/abuse", "<wanted wanted=\"maybe\"></wanted>"},
        {"m/abuse", "<wanted ignored=\"maybe\" wavelet_id=\"not an id\"></wanted>"},
        {"m/other", "<anything x=\"y\">text</anything>"},
    };
    for (String[] doc : udw) {
      validate(UDW, doc[0], doc[1]);
    }
    validate(ROOT, "roles", "<assign address=\"a@b.c\" role=\"FULL\"></assign>"
        + "<assign address=\"d@b.c\" role=\"READ_ONLY\"></assign>");
    validate(ROOT, "roles", "<assign address=\"a@b.c\" role=\"OWNER\"></assign>");
    validate(ROOT, "roles", "<assign>text</assign>");
    validate(UDW, "roles", "<assign role=\"OWNER\"></assign>");
  }

  public static void main(String[] args) throws Exception {
    out = new PrintStream(System.out, false, "UTF-8");
    Blips.init();
    adtScenarios();
    for (boolean fresh : new boolean[] {false, true}) {
      readScenario(fresh);
      inboxScenario(fresh);
      followScenario(fresh);
      multiScenario(fresh);
      miscScenario(fresh);
      corruptScenario(fresh);
    }
    foldersScenario();
    clearDuplicatesScenario();
    participantScenario();
    schemaScenarios();
    out.flush();
  }
}
