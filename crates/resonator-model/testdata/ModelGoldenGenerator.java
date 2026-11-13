// Generates model_golden.txt from the original Apache Wave Java
// implementation, for the differential tests of the doc, conversation and
// supplement modules (src/model_golden_tests.rs).
//
// Each scenario drives the reference model classes (MutableDocument over an
// OpBasedWavelet, WaveBasedConversationView, WaveletBasedSupplement,
// SupplementedWaveImpl) through a scripted sequence of steps. The Rust test
// runs the same script against the port and expects the same records.
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
//       crates/resonator-model/testdata/ModelGoldenGenerator.java
//   java -cp $T/out:$PROTOBUF_JAR:$GUAVA_JAR ModelGoldenGenerator \
//       > crates/resonator-model/testdata/model_golden.txt
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
//   D <wavelet id> <doc id> <lmv> <xml>
//                                after the step, every non-empty document of
//                                every wavelet, ordered by wavelet and doc id,
//                                with its last-modified version
//   P <wavelet id> <version> <participants>
//                                after the step, every wavelet
//   C <count>                    instead of O records, for steps whose order
//                                of operations depends on Java hash maps: the
//                                number of operations emitted
//   E                            the step threw
//   Q <text>                     an answer to a query made by the step
//
// Every operation is applied with a version increment of zero and followed
// by a version update of one, as OpBasedWaveletFactory does in the
// reference tests. Time is 1234567890123 ms plus one second per step so far
// (the current one included), plus one millisecond per operation the
// wavelet has emitted before in the current step; a wavelet is created at
// the time its step started.

import org.waveprotocol.wave.model.conversation.Blips;
import org.waveprotocol.wave.model.conversation.Conversation;
import org.waveprotocol.wave.model.conversation.ConversationBlip;
import org.waveprotocol.wave.model.conversation.ConversationThread;
import org.waveprotocol.wave.model.conversation.ObservableConversation;
import org.waveprotocol.wave.model.conversation.ObservableConversationBlip;
import org.waveprotocol.wave.model.conversation.ObservableConversationThread;
import org.waveprotocol.wave.model.conversation.TitleHelper;
import org.waveprotocol.wave.model.conversation.WaveBasedConversationView;
import org.waveprotocol.wave.model.document.Doc;
import org.waveprotocol.wave.model.document.Document;
import org.waveprotocol.wave.model.document.operation.Attributes;
import org.waveprotocol.wave.model.document.operation.DocOp;
import org.waveprotocol.wave.model.document.operation.Nindo;
import org.waveprotocol.wave.model.document.operation.impl.AttributesImpl;
import org.waveprotocol.wave.model.document.operation.impl.DocOpUtil;
import org.waveprotocol.wave.model.document.util.LineContainers;
import org.waveprotocol.wave.model.document.util.Point;
import org.waveprotocol.wave.model.document.util.XmlStringBuilder;
import org.waveprotocol.wave.model.id.IdGenerator;
import org.waveprotocol.wave.model.id.IdGeneratorImpl;
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
import org.waveprotocol.wave.model.version.HashedVersion;
import org.waveprotocol.wave.model.wave.ParticipantId;
import org.waveprotocol.wave.model.wave.ParticipationHelper;
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
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.TreeSet;

public class ModelGoldenGenerator {
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

  interface Step {
    void run() throws Exception;
  }

  /** A wave: wavelets that capture their operations. */
  static class Wave implements WaveViewImpl.WaveletFactory<OpBasedWavelet> {
    final ParticipantId author;
    final TreeMap<String, OpBasedWavelet> wavelets = new TreeMap<String, OpBasedWavelet>();
    final List<String[]> captured = new ArrayList<String[]>();
    boolean unordered = false;
    /** The time at the start of the current step. */
    long stepBase = TIME;
    /** Per wavelet, the number of operations emitted in the current step. */
    final List<long[]> clocks = new ArrayList<long[]>();

    Wave(ParticipantId author) {
      this.author = author;
    }

    @Override
    public OpBasedWavelet create(WaveId waveId, final WaveletId waveletId, ParticipantId creator) {
      final ObservableWaveletData data = WaveletDataImpl.Factory.create(
              ObservablePluggableMutableDocument.createFactory(SchemaCollection.empty()))
          .create(new EmptyWaveletSnapshot(waveId, waveletId, creator,
              HashedVersion.unsigned(0), stepBase));
      final long[] clock = new long[] {0};
      clocks.add(clock);
      WaveletOperationContext.Factory contexts = new WaveletOperationContext.Factory() {
        @Override
        public WaveletOperationContext createContext() {
          return new WaveletOperationContext(author, stepBase + clock[0], 0L);
        }

        @Override
        public WaveletOperationContext createContext(ParticipantId creator) {
          return new WaveletOperationContext(creator, stepBase + clock[0], 0L);
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
          clock[0]++;
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

    /**
     * A step of which only the number of operations is recorded: their order,
     * and with it their coordinates, depends on hash maps.
     */
    void stepUnordered(String label, Step step) {
      unordered = true;
      step(label, step);
      unordered = false;
    }

    void step(String label, Step step) {
      line("S", label);
      stepBase += 1000;
      for (long[] clock : clocks) {
        clock[0] = 0;
      }
      boolean failed = false;
      try {
        step.run();
      } catch (Exception e) {
        failed = true;
        if (System.getenv("GOLDEN_DEBUG") != null) e.printStackTrace();
      } catch (AssertionError e) {
        failed = true;
        if (System.getenv("GOLDEN_DEBUG") != null) e.printStackTrace();
      }
      // Stable: operations of one wavelet stay in emission order.
      Collections.sort(captured, (a, b) -> a[0].compareTo(b[0]));
      if (unordered) {
        line("C", captured.size());
        captured.clear();
      }
      for (String[] op : captured) {
        line("O", op[0], op[1]);
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
          String xml = DocOpUtil.toXmlString(doc.getContent().asOperation());
          if (!xml.isEmpty()) {
            line("D", e.getKey(), docId, doc.getLastModifiedVersion(), xml);
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

  static void q(Object answer) {
    line("Q", answer);
  }

  static Map<String, String> attrs(String... pairs) {
    Map<String, String> map = new LinkedHashMap<String, String>();
    for (int i = 0; i < pairs.length; i += 2) {
      map.put(pairs[i], pairs[i + 1]);
    }
    return map;
  }

  static Doc.E elementAt(Document doc, int location) {
    return Point.elementAfter(doc, doc.locate(location));
  }

  // ---- Document layer ----

  static void docScenarios() {
    line("N", "doc-basic");
    {
      final Wave wave = new Wave(ALICE);
      final Document doc = wave.wavelet("conv+root").getDocument("b+doc");
      wave.step("insert body", () -> doc.insertXml(Point.<Doc.N>end(doc.getDocumentElement()),
          XmlStringBuilder.createEmpty().wrap("line").wrap("body")));
      wave.step("insert text", () -> doc.insertText(3, "Hello world"));
      wave.step("bold", () -> doc.setAnnotation(3, 8, "style/fontWeight", "bold"));
      wave.step("link", () -> doc.setAnnotation(5, 12, "link/manual", "http://x"));
      wave.step("insert in annotated", () -> doc.insertText(6, "XY"));
      wave.step("insert at annotation end", () -> doc.insertText(10, "Z"));
      wave.step("create reply anchor", () -> doc.createElement(doc.locate(9), "reply",
          attrs("id", "t1")));
      wave.step("set attribute", () -> doc.setElementAttribute(elementAt(doc, 9), "id", "t2"));
      wave.step("set same attribute", () -> doc.setElementAttribute(elementAt(doc, 9), "id", "t2"));
      wave.step("update attributes", () -> {
        Map<String, String> updates = attrs("a", "1", "id", null, "zz", "z");
        doc.updateElementAttributes(elementAt(doc, 9), updates);
      });
      wave.step("replace attributes", () -> doc.setElementAttributes(elementAt(doc, 9),
          new AttributesImpl(attrs("z", "9", "b", "2"))));
      wave.step("unbold part", () -> doc.setAnnotation(4, 7, "style/fontWeight", null));
      wave.step("rebold overlapping", () -> doc.setAnnotation(2, 13, "style/fontWeight", "bold"));
      wave.step("set same annotation", () -> doc.setAnnotation(3, 6, "style/fontWeight", "bold"));
      wave.step("delete range over annotations", () -> doc.deleteRange(4, 13));
      wave.step("explicit title", () -> doc.resetAnnotation(0, 1, "conv/title", "Title"));
      wave.step("implicit title", () -> doc.resetAnnotation(1, 6, "conv/title", ""));
      wave.step("append line", () -> LineContainers.appendLine(doc,
          XmlStringBuilder.createText("second < line & more")));
      wave.step("append to last line", () -> LineContainers.appendToLastLine(doc,
          XmlStringBuilder.createText(" tail")));
      wave.step("insert empty text", () -> doc.insertText(4, ""));
      wave.step("delete empty range", () -> doc.deleteRange(4, 4));
      wave.step("delete line element", () -> doc.deleteNode(elementAt(doc, 1)));
      wave.step("delete all text", () -> doc.deleteRange(1, doc.size() - 1));
      wave.step("empty document", () -> doc.emptyElement(doc.getDocumentElement()));
      wave.step("append line to empty", () -> LineContainers.appendLine(doc,
          XmlStringBuilder.createText("fresh")));
      wave.step("append to last line of empty", () -> {
        doc.emptyElement(doc.getDocumentElement());
        LineContainers.appendToLastLine(doc, XmlStringBuilder.createText("x"));
      });
      wave.step("bad location", () -> doc.insertText(1000, "x"));
    }

    line("N", "doc-delete-across-elements");
    {
      final Wave wave = new Wave(ALICE);
      final Document doc = wave.wavelet("conv+root").getDocument("b+doc");
      wave.step("build", () -> doc.insertXml(Point.<Doc.N>end(doc.getDocumentElement()),
          XmlStringBuilder.createFromXmlString(
              "<a>one<b>two<c>three</c>four</b>five</a><d>six<e>seven</e>eight</d><f>nine</f>")));
      wave.step("annotate", () -> doc.setAnnotation(6, 30, "k", "v"));
      wave.step("annotate 2", () -> doc.setAnnotation(10, 20, "j", "w"));
      // From inside <c> to inside <e>.
      wave.step("delete c to e", () -> doc.deleteRange(11, 36));
      wave.step("delete a to f", () -> doc.deleteRange(2, doc.size() - 3));
      wave.step("delete into child", () -> doc.deleteRange(1, 5));
    }

    line("N", "doc-nindo");
    {
      final Wave wave = new Wave(ALICE);
      final Document doc = wave.wavelet("conv+root").getDocument("b+doc");
      wave.step("initial content with annotations",
          () -> doc.hackConsume(Nindo.fromDocOp(TitleHelper.emptyDocumentWithTitle(), false)));
      wave.step("text", () -> doc.insertText(3, "abcdefghij"));
      wave.step("mixed", () -> {
        Nindo.Builder b = new Nindo.Builder();
        b.skip(4);
        b.startAnnotation("x", "1");
        b.skip(2);
        b.characters("NEW");
        b.skip(1);
        b.endAnnotation("x");
        b.deleteCharacters(2);
        b.startAnnotation("y", "2");
        b.skip(2);
        b.endAnnotation("y");
        doc.hackConsume(b.build());
      });
      wave.step("delete over three annotations", () -> doc.deleteRange(2, 12));
      wave.step("remove annotation everywhere", () -> doc.setAnnotation(0, doc.size(), "x", null));
      wave.step("delete after key removed", () -> doc.deleteRange(3, 4));
    }

    line("N", "doc-unicode");
    {
      final Wave wave = new Wave(ALICE);
      final Document doc = wave.wavelet("conv+root").getDocument("b+doc");
      wave.step("build", () -> doc.insertXml(Point.<Doc.N>end(doc.getDocumentElement()),
          XmlStringBuilder.createEmpty().wrap("line").wrap("body")));
      wave.step("text", () -> doc.insertText(3, "héllo 世界 \"q\" 'a' <&>"));
      wave.step("annotate", () -> doc.setAnnotation(4, 9, "kéy", "väl\"ue"));
      wave.step("delete", () -> doc.deleteRange(5, 11));
    }
  }

  // ---- Conversation model ----

  static IdGenerator ids() {
    return new IdGeneratorImpl("example.com", new IdGeneratorImpl.Seed() {
      @Override
      public String get() {
        return "seed";
      }
    });
  }

  static WaveBasedConversationView conversations(Wave wave) {
    IdGenerator ids = ids();
    WaveViewImpl<OpBasedWavelet> view = WaveViewImpl.create(wave, WAVE, ids, wave.author,
        WaveViewImpl.WaveletConfigurator.ADD_CREATOR);
    return WaveBasedConversationView.create(view, ids);
  }

  static ObservableConversation root(WaveBasedConversationView convs) {
    return convs.getRoot();
  }

  static String dump(ObservableConversationThread thread) {
    StringBuilder b = new StringBuilder();
    b.append("T(").append(thread.getId()).append(")[");
    boolean first = true;
    for (ObservableConversationBlip blip : thread.getBlips()) {
      if (!first) b.append(",");
      first = false;
      b.append(blip.getId());
      if (blip.isRoot()) b.append("^");
      b.append("{");
      boolean firstReply = true;
      for (ConversationBlip.LocatedReplyThread<? extends ObservableConversationThread> reply
          : blip.locateReplyThreads()) {
        if (!firstReply) b.append(",");
        firstReply = false;
        b.append(reply.getLocation()).append(":").append(dump(reply.getThread()));
      }
      b.append("}");
    }
    return b.append("]").toString();
  }

  static void queryConversation(ObservableConversation c) {
    q("structure " + dump(c.getRootThread()));
    StringBuilder order = new StringBuilder("bfs");
    for (ConversationBlip blip : org.waveprotocol.wave.model.conversation.BlipIterators
        .breadthFirst(c)) {
      order.append(' ').append(blip.getId());
    }
    q(order);
    ObservableConversationBlip first = c.getRootThread().getFirstBlip();
    q("title " + (first == null ? "" : TitleHelper.extractTitle(first.getContent())));
  }

  static void queryBlip(ObservableConversationBlip blip) {
    TreeSet<String> contributors = new TreeSet<String>();
    for (ParticipantId p : blip.getContributorIds()) {
      contributors.add(p.getAddress());
    }
    q("blip " + blip.getId() + " author " + blip.getAuthorId().getAddress() + " contributors "
        + contributors + " lmv " + blip.getLastModifiedVersion() + " lmt "
        + blip.getLastModifiedTime());
  }

  static void conversationScenarios() {
    line("N", "conv-basic");
    {
      final Wave wave = new Wave(ALICE);
      final WaveBasedConversationView convs = conversations(wave);
      wave.step("create root", () -> convs.createRoot());
      final ObservableConversation c = convs.getRoot();
      wave.step("query empty", () -> {
        q("id " + c.getId());
        q("view " + convs.getId());
        queryConversation(c);
      });
      final String[] id = new String[10];
      wave.step("append root blip", () -> id[1] = c.getRootThread().appendBlip().getId());
      wave.step("write text", () -> c.getBlip(id[1]).getContent()
          .insertText(3, "Hello world. More text"));
      wave.step("implicit title", () -> {
        TitleHelper.maybeFindAndSetImplicitTitle(c.getBlip(id[1]).getContent());
        queryConversation(c);
      });
      wave.step("implicit title again", () ->
          TitleHelper.maybeFindAndSetImplicitTitle(c.getBlip(id[1]).getContent()));
      wave.step("append second blip", () -> id[2] = c.getRootThread().appendBlip().getId());
      final String[] thread = new String[10];
      wave.step("reply thread", () -> thread[1] = c.getBlip(id[1]).addReplyThread().getId());
      wave.step("reply blip", () -> id[3] = c.getThread(thread[1]).appendBlip().getId());
      wave.step("inline reply thread",
          () -> thread[2] = c.getBlip(id[1]).addReplyThread(8).getId());
      wave.step("inline reply blips", () -> {
        id[4] = c.getThread(thread[2]).appendBlip().getId();
        id[5] = c.getThread(thread[2]).appendBlip().getId();
      });
      wave.step("second inline reply thread, earlier", () -> {
        thread[3] = c.getBlip(id[1]).addReplyThread(5).getId();
        id[6] = c.getThread(thread[3]).appendBlip().getId();
      });
      wave.step("nested reply", () -> {
        thread[4] = c.getBlip(id[3]).addReplyThread().getId();
        id[7] = c.getThread(thread[4]).appendBlip().getId();
      });
      wave.step("insert blip", () -> id[8] = c.getRootThread().insertBlip(c.getBlip(id[2]))
          .getId());
      wave.step("query", () -> {
        queryConversation(c);
        for (int i = 1; i <= 8; i++) {
          queryBlip(c.getBlip(id[i]));
        }
      });
      wave.step("explicit title", () -> {
        TitleHelper.setExplicitTitle(c.getBlip(id[1]).getContent(), "Explicit");
        queryConversation(c);
      });
      wave.step("implicit title with explicit present", () ->
          TitleHelper.maybeFindAndSetImplicitTitle(c.getBlip(id[1]).getContent()));
      wave.step("participants", () -> {
        c.addParticipant(BOB);
        c.addParticipant(BOB);
        c.removeParticipant(ALICE);
        c.removeParticipant(ALICE);
      });
      wave.step("author is no participant", () -> c.getBlip(id[2]).getContent()
          .insertText(3, "x"));
      wave.step("delete first inline blip", () -> c.getBlip(id[4]).delete());
      wave.step("delete last inline blip", () -> {
        c.getBlip(id[5]).delete();
        queryConversation(c);
      });
      wave.step("delete nested thread", () -> c.getThread(thread[4]).delete());
      wave.step("delete reply thread", () -> c.getThread(thread[1]).delete());
      wave.step("delete inserted blip", () -> c.getBlip(id[8]).delete());
      wave.step("delete blip with inline reply", () -> {
        c.getBlip(id[1]).delete();
        queryConversation(c);
      });
      wave.step("tags", () -> {
        org.waveprotocol.wave.model.conversation.TagsDocument<Doc.N, Doc.E, Doc.T> tags =
            new org.waveprotocol.wave.model.conversation.TagsDocument<Doc.N, Doc.E, Doc.T>(
                c.getDataDocument("tags"));
        tags.addTag("one");
        tags.addTag("two & <three>");
        tags.addTag("four");
        tags.deleteTag("two & <three>");
        tags.deleteTag("missing");
        tags.addTag("zero", 0);
      });
      wave.step("delete conversation", () -> c.delete());
    }

    line("N", "conv-delete-many");
    {
      final Wave wave = new Wave(ALICE);
      final WaveBasedConversationView convs = conversations(wave);
      final String[] id = new String[10];
      final String[] thread = new String[10];
      wave.step("build", () -> {
        ObservableConversation c = convs.createRoot();
        id[1] = c.getRootThread().appendBlip().getId();
        c.getBlip(id[1]).getContent().insertText(3, "Some text here");
        thread[1] = c.getBlip(id[1]).addReplyThread(5).getId();
        id[2] = c.getThread(thread[1]).appendBlip().getId();
        id[3] = c.getThread(thread[1]).appendBlip().getId();
        thread[2] = c.getBlip(id[1]).addReplyThread().getId();
        id[4] = c.getThread(thread[2]).appendBlip().getId();
        thread[3] = c.getBlip(id[1]).addReplyThread(9).getId();
        id[5] = c.getThread(thread[3]).appendBlip().getId();
        thread[4] = c.getBlip(id[3]).addReplyThread(4).getId();
        id[6] = c.getThread(thread[4]).appendBlip().getId();
        id[7] = c.getRootThread().appendBlip().getId();
        queryConversation(c);
      });
      wave.stepUnordered("delete blip with many replies", () -> {
        root(convs).getBlip(id[1]).delete();
        queryConversation(convs.getRoot());
      });
      wave.stepUnordered("delete root thread", () -> {
        root(convs).getRootThread().delete();
        queryConversation(convs.getRoot());
      });
    }

    line("N", "conv-anchor");
    {
      final Wave wave = new Wave(ALICE);
      final WaveBasedConversationView convs = conversations(wave);
      final String[] id = new String[10];
      wave.step("create root", () -> {
        ObservableConversation c = convs.createRoot();
        id[1] = c.getRootThread().appendBlip().getId();
      });
      final ObservableConversation[] other = new ObservableConversation[1];
      wave.step("create conversation", () -> {
        other[0] = convs.createConversation();
        id[2] = other[0].getRootThread().appendBlip().getId();
        q("id " + other[0].getId());
        q("has anchor " + other[0].hasAnchor());
      });
      wave.step("set anchor", () -> {
        other[0].setAnchor(convs.getRoot().createAnchor(convs.getRoot().getBlip(id[1])));
        q("has anchor " + other[0].hasAnchor());
        q("anchor " + other[0].getAnchor().getConversation().getId() + " "
            + other[0].getAnchor().getBlip().getId());
      });
      wave.step("set same anchor", () ->
        other[0].setAnchor(convs.getRoot().createAnchor(convs.getRoot().getBlip(id[1]))));
      wave.step("delete anchor blip", () -> {
        root(convs).getBlip(id[1]).delete();
        q("has anchor " + other[0].hasAnchor());
      });
      wave.step("clear anchor", () -> {
        other[0].setAnchor(null);
        q("has anchor " + other[0].hasAnchor());
      });
      wave.step("append with content", () -> other[0].getRootThread()
          .appendBlip(TitleHelper.emptyDocumentWithTitle()));
    }

    line("N", "wavelet-authorise");
    {
      final Wave wave = new Wave(BOB);
      final OpBasedWavelet wavelet = wave.wavelet("conv+root");
      wave.step("add alice to empty wavelet", () -> wavelet.addParticipant(ALICE));
      wave.step("edit as non-participant", () -> wavelet.getDocument("b+x").insertXml(
          Point.<Doc.N>end(wavelet.getDocument("b+x").getDocumentElement()),
          XmlStringBuilder.createEmpty().wrap("line").wrap("body")));
      wave.step("remove bob", () -> wavelet.removeParticipant(BOB));
      wave.step("add self as non-participant", () -> wavelet.addParticipant(BOB));
      wave.step("remove bob again", () -> wavelet.removeParticipant(BOB));
      wave.step("remove alice as non-participant", () -> wavelet.removeParticipant(ALICE));
      wave.step("touch empty wavelet", () -> wavelet.touch());
    }
  }

  // ---- Digests ----

  static void digest(Wave wave, ParticipantId viewer) {
    org.waveprotocol.wave.model.wave.data.impl.WaveViewDataImpl view =
        org.waveprotocol.wave.model.wave.data.impl.WaveViewDataImpl.create(WAVE);
    for (OpBasedWavelet wavelet : wave.wavelets.values()) {
      // A copy, as the server hands the digester: a wavelet's documents can
      // be wrapped only once.
      view.addWavelet(org.waveprotocol.box.server.util.WaveletDataUtil.copyWavelet(
          wavelet.getWaveletData()));
    }
    org.waveprotocol.box.server.waveserver.WaveDigester digester =
        new org.waveprotocol.box.server.waveserver.WaveDigester(
            new org.waveprotocol.box.server.robots.util.ConversationUtil(ids()));
    com.google.wave.api.SearchResult.Digest d = digester.build(viewer, view);
    q("digest title [" + d.getTitle() + "] snippet [" + d.getSnippet() + "] participants "
        + d.getParticipants() + " modified " + d.getLastModified() + " created " + d.getCreated()
        + " unread " + d.getUnreadCount() + " blips " + d.getBlipCount());
  }

  static org.waveprotocol.wave.model.supplement.SupplementedWave supplement(Wave wave,
      WaveBasedConversationView convs, ParticipantId viewer) {
    OpBasedWavelet udw = wave.wavelet("user+" + viewer.getAddress());
    return org.waveprotocol.wave.model.supplement.SupplementedWaveImpl.create(
        org.waveprotocol.wave.model.supplement.WaveletBasedSupplement.create(udw), convs, viewer,
        org.waveprotocol.wave.model.supplement.SupplementedWaveImpl.DefaultFollow.ALWAYS);
  }

  static void digestScenarios() {
    line("N", "digest");
    {
      final Wave wave = new Wave(ALICE);
      final WaveBasedConversationView convs = conversations(wave);
      final String[] id = new String[10];
      final String[] thread = new String[10];
      wave.step("empty wave", () -> digest(wave, ALICE));
      wave.step("non-conversational wavelet", () -> {
        wave.wavelet("conv+root").getDocument("some-doc").insertText(0, "hello");
        digest(wave, ALICE);
      });
      wave.step("clear", () -> wave.wavelet("conv+root").getDocument("some-doc")
          .deleteRange(0, 5));
      wave.step("empty conversation", () -> {
        WaveBasedConversationView.create(
            org.waveprotocol.wave.model.wave.opbased.WaveViewImpl.create(wave, WAVE, ids(), ALICE,
                WaveViewImpl.WaveletConfigurator.NONE), ids());
        org.waveprotocol.wave.model.conversation.WaveletBasedConversation
            .makeWaveletConversational(wave.wavelet("conv+root"));
        digest(wave, ALICE);
      });
    }

    line("N", "digest-conversation");
    {
      final Wave wave = new Wave(ALICE);
      final WaveBasedConversationView convs = conversations(wave);
      final String[] id = new String[10];
      final String[] thread = new String[10];
      wave.step("root blip", () -> {
        ObservableConversation c = convs.createRoot();
        id[1] = c.getRootThread().appendBlip().getId();
        digest(wave, ALICE);
      });
      final ObservableConversation c = root(convs);
      wave.step("text", () -> {
        c.getBlip(id[1]).getContent().insertText(3, "  The title.  Some body text ");
        digest(wave, ALICE);
      });
      wave.step("implicit title", () -> {
        TitleHelper.maybeFindAndSetImplicitTitle(c.getBlip(id[1]).getContent());
        digest(wave, ALICE);
      });
      wave.step("more lines and blips", () -> {
        LineContainers.appendLine(c.getBlip(id[1]).getContent(),
            XmlStringBuilder.createText("Second line"));
        id[2] = c.getRootThread().appendBlip().getId();
        c.getBlip(id[2]).getContent().insertText(3, "Second blip é世");
        thread[1] = c.getBlip(id[1]).addReplyThread(6).getId();
        id[3] = c.getThread(thread[1]).appendBlip().getId();
        c.getBlip(id[3]).getContent().insertText(3, "Inline reply");
        c.addParticipant(BOB);
        digest(wave, ALICE);
        digest(wave, BOB);
      });
      wave.step("edit old blip", () -> {
        c.getBlip(id[2]).getContent().insertText(3, "Edited. ");
        digest(wave, ALICE);
      });
      wave.step("long text", () -> {
        StringBuilder text = new StringBuilder();
        for (int i = 0; i < 30; i++) {
          text.append("word").append(i).append(' ');
        }
        c.getBlip(id[3]).getContent().insertText(3, text.toString());
        digest(wave, ALICE);
      });
      wave.step("mark blip read", () -> {
        supplement(wave, convs, ALICE).markAsRead(c.getBlip(id[1]));
        digest(wave, ALICE);
        digest(wave, BOB);
      });
      wave.step("edit read blip", () -> {
        c.getBlip(id[1]).getContent().insertText(3, "Now ");
        digest(wave, ALICE);
      });
      wave.step("mark wave read", () -> {
        supplement(wave, convs, ALICE).markAsRead();
        digest(wave, ALICE);
      });
      wave.step("new blip", () -> {
        id[4] = c.getRootThread().appendBlip().getId();
        digest(wave, ALICE);
      });
      wave.step("explicit title", () -> {
        TitleHelper.setExplicitTitle(c.getBlip(id[1]).getContent(), "Now  The");
        digest(wave, ALICE);
      });
      wave.step("many participants", () -> {
        for (String name : new String[] {"c", "d", "e", "f", "g"}) {
          c.addParticipant(new ParticipantId(name + "@example.com"));
        }
        digest(wave, ALICE);
      });
      wave.step("mark unread", () -> {
        supplement(wave, convs, ALICE).markAsUnread();
        digest(wave, ALICE);
      });
      wave.step("delete blips", () -> {
        c.getBlip(id[2]).delete();
        c.getBlip(id[1]).delete();
        digest(wave, ALICE);
      });
    }

    line("N", "digest-other-conversation");
    {
      final Wave wave = new Wave(ALICE);
      final WaveBasedConversationView convs = conversations(wave);
      wave.step("non-root conversation", () -> {
        ObservableConversation c = convs.createConversation();
        c.getRootThread().appendBlip().getContent().insertText(3, "Private reply");
        try {
          digest(wave, ALICE);
        } catch (NullPointerException e) {
          q("digest fails");
        }
      });
    }
  }

  public static void main(String[] args) throws Exception {
    out = new PrintStream(System.out, false, "UTF-8");
    Blips.init();
    docScenarios();
    conversationScenarios();
    digestScenarios();
    out.flush();
  }
}
