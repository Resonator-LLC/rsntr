// Generates api_golden.txt from the original Apache Wave Java implementation,
// for the differential tests of resonator-robots (tests/golden.rs).
//
// It drives the reference classes of com.google.wave.api and
// org.waveprotocol.box.server.robots: the Gson serializers, the event data
// converters with ApiView and ElementSerializer, the operation services
// (through a small OperationContext of its own) and the EventGenerator.
//
// To regenerate (from the workspace root, with a JDK and the Guava, Gson,
// commons-codec and javax.inject jars; compile in a scratch directory,
// never in the reference tree):
//
//   W=reference/apache-wave/wave/src; T=$(mktemp -d)
//   mkdir -p $T/stubs/javax/annotation $T/stubs/com/google/inject \
//       $T/stubs/org/waveprotocol/box/server/robots/util
//   echo 'package javax.annotation; public @interface Nullable {}' \
//       > $T/stubs/javax/annotation/Nullable.java
//   echo 'package com.google.inject; public @interface Inject {}' \
//       > $T/stubs/com/google/inject/Inject.java
//   # OperationUtil without submitDeltas and buildSupplement, which need the
//   # wave server and generated protobuf classes:
//   python3 crates/resonator-robots/testdata/strip_operation_util.py \
//       $W/main/java/org/waveprotocol/box/server/robots/util/OperationUtil.java \
//       > $T/stubs/org/waveprotocol/box/server/robots/util/OperationUtil.java
//   # With a Guava that no longer has CharMatcher.WHITESPACE (21 or later):
//   mkdir -p $T/stubs/com/google/wave/api/data
//   sed 's/CharMatcher.WHITESPACE/CharMatcher.whitespace()/' \
//       $W/main/java/com/google/wave/api/data/ElementSerializer.java \
//       > $T/stubs/com/google/wave/api/data/ElementSerializer.java
//   CP=$GUAVA_JAR:$GSON_JAR:$COMMONS_CODEC_JAR:$JAVAX_INJECT_JAR
//   javac -nowarn -cp $CP -d $T/out -sourcepath $T/stubs:$W/main/java \
//       crates/resonator-robots/testdata/ApiGoldenGenerator.java
//   java -cp $T/out:$CP ApiGoldenGenerator > crates/resonator-robots/testdata/api_golden.txt
//
// Output: one record per line, tab-separated, first field a tag; characters
// outside printable ASCII and backslashes are written \\uXXXX.
//
//   J <kind> <name> <json>     something serialized by GsonFactory; kind is
//                              REQS, RESPS, BUNDLE or BUNDLE02 (protocol 0.2)
//   B <name>                   a wavelet state follows:
//   W <creator> <creationTime> <version> <lastModifiedTime> <participants>
//   D <docId> <author> <contributors> <lmt> <lmv> <op components...>
//   V <converter> W <json>     toWaveletData of the state
//   V <converter> B <blipId> <json>   toBlipData of each blip
//   V <converter> T <json>     the threads the conversions filed
//   A <blipId> <apiContents>   ApiView of each blip, and for the same blip
//   AX <blipId> <xml offsets of api positions 0..len+1>
//   AT <blipId> <text offsets of xml locations 0..size>
//   SVC <name> <base> <actor> <request json>   an operation executed on a
//                              fresh copy of state <base>, followed by
//   OP ...                     the wavelet operations it emitted
//   RESP <json> | NORESP | IRE <message> | EXC <class>   and how it ended
//   EV <name> <base> <robot> <protocol> <capabilities>   event generation;
//                              <base> is the state AFTER the deltas, then
//   DELTA <author> <resultingVersion> <timestamp>  each followed by its
//   OP ...                     operations, and finally
//   BUNDLE <json>
//
// OP records: "OP add <address>", "OP remove <address>", "OP noop" or
// "OP doc <docId> <components...>". A component is one field, its parts
// separated by U+0001, null written U+0002: r,n | c,text | s,tag,k,v,... |
// e | dc,text | ds,tag,k,v,... | de | ra,nOld,k,v,...,k,v,... |
// ua,k,old,new,... | ab,nEnd,endKeys...,k,old,new,...
// Document operations are normalized.

import com.google.common.collect.Lists;
import com.google.common.collect.Maps;
import com.google.gson.Gson;
import com.google.wave.api.Annotation;
import com.google.wave.api.ApiIdSerializer;
import com.google.wave.api.Attachment;
import com.google.wave.api.BlipData;
import com.google.wave.api.BlipThread;
import com.google.wave.api.Context;
import com.google.wave.api.Element;
import com.google.wave.api.ElementType;
import com.google.wave.api.FetchProfilesRequest;
import com.google.wave.api.FetchProfilesResult;
import com.google.wave.api.FormElement;
import com.google.wave.api.Gadget;
import com.google.wave.api.Image;
import com.google.wave.api.InvalidRequestException;
import com.google.wave.api.JsonRpcConstant.ParamsProperty;
import com.google.wave.api.JsonRpcResponse;
import com.google.wave.api.Line;
import com.google.wave.api.OperationRequest;
import com.google.wave.api.OperationRequest.Parameter;
import com.google.wave.api.OperationType;
import com.google.wave.api.ParticipantProfile;
import com.google.wave.api.ProtocolVersion;
import com.google.wave.api.Range;
import com.google.wave.api.SearchResult;
import com.google.wave.api.data.ApiView;
import com.google.wave.api.data.converter.EventDataConverter;
import com.google.wave.api.data.converter.v21.EventDataConverterV21;
import com.google.wave.api.data.converter.v22.EventDataConverterV22;
import com.google.wave.api.event.AnnotatedTextChangedEvent;
import com.google.wave.api.event.BlipContributorsChangedEvent;
import com.google.wave.api.event.BlipSubmittedEvent;
import com.google.wave.api.event.DocumentChangedEvent;
import com.google.wave.api.event.Event;
import com.google.wave.api.event.EventSerializationException;
import com.google.wave.api.event.EventSerializer;
import com.google.wave.api.event.EventType;
import com.google.wave.api.event.FormButtonClickedEvent;
import com.google.wave.api.event.GadgetStateChangedEvent;
import com.google.wave.api.event.OperationErrorEvent;
import com.google.wave.api.event.WaveletBlipCreatedEvent;
import com.google.wave.api.event.WaveletBlipRemovedEvent;
import com.google.wave.api.event.WaveletCreatedEvent;
import com.google.wave.api.event.WaveletFetchedEvent;
import com.google.wave.api.event.WaveletParticipantsChangedEvent;
import com.google.wave.api.event.WaveletSelfAddedEvent;
import com.google.wave.api.event.WaveletSelfRemovedEvent;
import com.google.wave.api.event.WaveletTagsChangedEvent;
import com.google.wave.api.event.WaveletTitleChangedEvent;
import com.google.wave.api.impl.DocumentModifyAction;
import com.google.wave.api.impl.DocumentModifyAction.BundledAnnotation;
import com.google.wave.api.impl.DocumentModifyAction.ModifyHow;
import com.google.wave.api.impl.DocumentModifyQuery;
import com.google.wave.api.impl.EventMessageBundle;
import com.google.wave.api.impl.GsonFactory;
import com.google.wave.api.impl.RawAttachmentData;
import com.google.wave.api.impl.WaveletData;
import com.google.wave.api.robot.Capability;
import com.google.wave.api.robot.RobotName;
import com.google.wave.api.v2.ElementGsonAdaptorV2;

import org.waveprotocol.box.common.DeltaSequence;
import org.waveprotocol.box.common.Receiver;
import org.waveprotocol.box.server.frontend.CommittedWaveletSnapshot;
import org.waveprotocol.box.server.robots.OperationContext;
import org.waveprotocol.box.server.robots.RobotWaveletData;
import org.waveprotocol.box.server.robots.operations.BlipOperationServices;
import org.waveprotocol.box.server.robots.operations.DocumentModifyService;
import org.waveprotocol.box.server.robots.operations.FetchWaveService;
import org.waveprotocol.box.server.robots.operations.OperationService;
import org.waveprotocol.box.server.robots.operations.ParticipantServices;
import org.waveprotocol.box.server.robots.operations.WaveletSetTitleService;
import org.waveprotocol.box.server.robots.passive.EventGenerator;
import org.waveprotocol.box.server.robots.passive.WaveletAndDeltas;
import org.waveprotocol.box.server.robots.util.ConversationUtil;
import org.waveprotocol.wave.model.conversation.ConversationBlip;
import org.waveprotocol.wave.model.conversation.ConversationThread;
import org.waveprotocol.wave.model.conversation.Conversation;
import org.waveprotocol.wave.model.conversation.ObservableConversation;
import org.waveprotocol.wave.model.conversation.ObservableConversationBlip;
import org.waveprotocol.wave.model.conversation.ObservableConversationView;
import org.waveprotocol.wave.model.conversation.TitleHelper;
import org.waveprotocol.wave.model.conversation.WaveletBasedConversation;
import org.waveprotocol.wave.model.document.Document;
import org.waveprotocol.wave.model.document.operation.AnnotationBoundaryMap;
import org.waveprotocol.wave.model.document.operation.Attributes;
import org.waveprotocol.wave.model.document.operation.AttributesUpdate;
import org.waveprotocol.wave.model.document.operation.DocOp;
import org.waveprotocol.wave.model.document.operation.DocOpCursor;
import org.waveprotocol.wave.model.document.operation.impl.DocOpUtil;
import org.waveprotocol.wave.model.document.util.LineContainers;
import org.waveprotocol.wave.model.document.util.XmlStringBuilder;
import org.waveprotocol.wave.model.id.IdGenerator;
import org.waveprotocol.wave.model.id.IdGeneratorImpl;
import org.waveprotocol.wave.model.id.WaveId;
import org.waveprotocol.wave.model.id.WaveletId;
import org.waveprotocol.wave.model.id.WaveletName;
import org.waveprotocol.wave.model.operation.OperationException;
import org.waveprotocol.wave.model.operation.SilentOperationSink;
import org.waveprotocol.wave.model.operation.wave.AddParticipant;
import org.waveprotocol.wave.model.operation.wave.BlipContentOperation;
import org.waveprotocol.wave.model.operation.wave.NoOp;
import org.waveprotocol.wave.model.operation.wave.RemoveParticipant;
import org.waveprotocol.wave.model.operation.wave.TransformedWaveletDelta;
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

import com.google.common.collect.ImmutableSet;

import java.io.PrintStream;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeSet;

public class ApiGoldenGenerator {
  static PrintStream out;
  static final char SEP = '\u0001';
  static final String NULL = "\u0002";

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
  static final ParticipantId ROBOT = new ParticipantId("robot@example.com");
  static final WaveId WAVE = WaveId.of("example.com", "w+wave");
  static final WaveletId WAVELET = WaveletId.of("example.com", "conv+root");
  static final String WAVE_S = "example.com/w+wave";
  static final String WAVELET_S = "example.com/conv+root";
  static final Gson GSON = new GsonFactory().create();

  static String nz(String s) {
    return s == null ? NULL : s;
  }

  // ---- Document operations ----

  static List<String> components(DocOp op) {
    final List<String> parts = new ArrayList<String>();
    DocOpUtil.normalize(op).apply(new DocOpCursor() {
      String attrs(Map<String, String> attrs) {
        StringBuilder b = new StringBuilder();
        for (Map.Entry<String, String> e : attrs.entrySet()) {
          b.append(SEP).append(e.getKey()).append(SEP).append(e.getValue());
        }
        return b.toString();
      }
      @Override public void annotationBoundary(AnnotationBoundaryMap map) {
        StringBuilder b = new StringBuilder("ab").append(SEP).append(map.endSize());
        for (int i = 0; i < map.endSize(); i++) {
          b.append(SEP).append(map.getEndKey(i));
        }
        for (int i = 0; i < map.changeSize(); i++) {
          b.append(SEP).append(map.getChangeKey(i)).append(SEP).append(nz(map.getOldValue(i)))
              .append(SEP).append(nz(map.getNewValue(i)));
        }
        parts.add(b.toString());
      }
      @Override public void characters(String chars) {
        parts.add("c" + SEP + chars);
      }
      @Override public void elementStart(String type, Attributes attrs) {
        parts.add("s" + SEP + type + attrs(attrs));
      }
      @Override public void elementEnd() {
        parts.add("e");
      }
      @Override public void retain(int itemCount) {
        parts.add("r" + SEP + itemCount);
      }
      @Override public void deleteCharacters(String chars) {
        parts.add("dc" + SEP + chars);
      }
      @Override public void deleteElementStart(String type, Attributes attrs) {
        parts.add("ds" + SEP + type + attrs(attrs));
      }
      @Override public void deleteElementEnd() {
        parts.add("de");
      }
      @Override public void replaceAttributes(Attributes oldAttrs, Attributes newAttrs) {
        parts.add("ra" + SEP + oldAttrs.size() + attrs(oldAttrs) + attrs(newAttrs));
      }
      @Override public void updateAttributes(AttributesUpdate update) {
        StringBuilder b = new StringBuilder("ua");
        for (int i = 0; i < update.changeSize(); i++) {
          b.append(SEP).append(update.getChangeKey(i)).append(SEP)
              .append(nz(update.getOldValue(i))).append(SEP).append(nz(update.getNewValue(i)));
        }
        parts.add(b.toString());
      }
    });
    return parts;
  }

  static void opLine(WaveletOperation op) {
    List<Object> fields = new ArrayList<Object>();
    fields.add("OP");
    if (op instanceof AddParticipant) {
      fields.add("add");
      fields.add(((AddParticipant) op).getParticipantId().getAddress());
    } else if (op instanceof RemoveParticipant) {
      fields.add("remove");
      fields.add(((RemoveParticipant) op).getParticipantId().getAddress());
    } else if (op instanceof NoOp) {
      fields.add("noop");
    } else if (op instanceof WaveletBlipOperation) {
      WaveletBlipOperation blipOp = (WaveletBlipOperation) op;
      fields.add("doc");
      fields.add(blipOp.getBlipId());
      fields.addAll(components(((BlipContentOperation) blipOp.getBlipOp()).getContentOp()));
    } else {
      throw new IllegalArgumentException("unexpected op " + op);
    }
    line(fields.toArray());
  }

  // ---- A wavelet that captures its operations ----

  static IdGenerator ids(final String seed) {
    return new IdGeneratorImpl("example.com", new IdGeneratorImpl.Seed() {
      @Override
      public String get() {
        return seed;
      }
    });
  }

  static class Fx {
    final ObservableWaveletData data;
    final OpBasedWavelet wavelet;
    final List<WaveletOperation> captured = new ArrayList<WaveletOperation>();
    ParticipantId author;
    long time = TIME;
    ObservableConversationView view;
    ObservableConversation conv;

    Fx(ParticipantId creator) {
      this.author = creator;
      data = WaveletDataImpl.Factory.create(
              ObservablePluggableMutableDocument.createFactory(SchemaCollection.empty()))
          .create(new EmptyWaveletSnapshot(WAVE, WAVELET, creator, HashedVersion.unsigned(0),
              TIME));
      WaveletOperationContext.Factory contexts = new WaveletOperationContext.Factory() {
        @Override
        public WaveletOperationContext createContext() {
          return new WaveletOperationContext(author, time, 0L);
        }

        @Override
        public WaveletOperationContext createContext(ParticipantId creator) {
          return new WaveletOperationContext(creator, time, 0L);
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
          captured.add(op);
        }
      };
      wavelet = new OpBasedWavelet(WAVE, data, contexts, ParticipationHelper.DEFAULT,
          SilentOperationSink.Executor.<WaveletOperation, ObservableWaveletData>build(data),
          output);
      wavelet.addParticipant(creator);
      WaveletBasedConversation.makeWaveletConversational(wavelet);
      reopen("base");
    }

    /** A new conversation view with a new source of ids. */
    void reopen(String seed) {
      view = new ConversationUtil(ids(seed)).buildConversation(wavelet);
      conv = view.getRoot();
    }

    Document doc(ConversationBlip blip) {
      return blip.getContent();
    }

    void append(ConversationBlip blip, String xml) {
      Document doc = blip.getContent();
      LineContainers.appendToLastLine(doc, XmlStringBuilder.createFromXmlString(xml));
    }

    void dump(String name) {
      line("B", name);
      StringBuilder participants = new StringBuilder();
      for (ParticipantId p : data.getParticipants()) {
        participants.append(p.getAddress()).append(' ');
      }
      line("W", data.getCreator().getAddress(), data.getCreationTime(), data.getVersion(),
          data.getLastModifiedTime(), participants.toString().trim());
      for (String docId : new TreeSet<String>(data.getDocumentIds())) {
        ReadableBlipData doc = data.getDocument(docId);
        StringBuilder contributors = new StringBuilder();
        for (ParticipantId p : doc.getContributors()) {
          contributors.append(p.getAddress()).append(' ');
        }
        List<Object> fields = new ArrayList<Object>();
        fields.addAll(Arrays.<Object>asList("D", docId, doc.getAuthor().getAddress(),
            contributors.toString().trim(), doc.getLastModifiedTime(),
            doc.getLastModifiedVersion()));
        fields.addAll(components(doc.getContent().asOperation()));
        line(fields.toArray());
      }
    }
  }

  // ---- Base states ----

  interface Builder {
    Fx build();
  }

  static final Map<String, Builder> BASES = new LinkedHashMap<String, Builder>();

  /** One blip with two lines of text. */
  static Fx simple() {
    Fx fx = new Fx(ALICE);
    fx.wavelet.addParticipant(BOB);
    ObservableConversationBlip root = fx.conv.getRootThread().appendBlip();
    fx.append(root, "Hello world");
    LineContainers.appendLine(root.getContent(), XmlStringBuilder.createText("second line foo"));
    LineContainers.appendLine(root.getContent(), XmlStringBuilder.createText("foo and foo"));
    return fx;
  }

  /** A conversation with replies, inline and not, and all sorts of content. */
  static Fx rich() {
    Fx fx = new Fx(ALICE);
    fx.wavelet.addParticipant(BOB);
    ObservableConversationBlip root = fx.conv.getRootThread().appendBlip();
    fx.append(root, "Title line");
    Document doc = root.getContent();
    TitleHelper.setExplicitTitle(doc, "The title");
    LineContainers.appendLine(doc, XmlStringBuilder.createText("styled and linked"),
        new org.waveprotocol.wave.model.document.operation.impl.AttributesImpl("t", "h1"));
    int bodyEnd = doc.size() - 1;
    doc.setAnnotation(bodyEnd - 10, bodyEnd - 4, "style/fontWeight", "bold");
    doc.setAnnotation(bodyEnd - 6, bodyEnd, "link/manual", "http://example.com/");
    LineContainers.appendLine(doc, XmlStringBuilder.createFromXmlString(
        "<gadget url=\"http://g.example/g.xml\" author=\"alice@example.com\" ifr=\"ifr1\">"
        + "<title value=\"Gadget\"/><thumbnail value=\"thumb\"/><category name=\"cat\"/>"
        + "<pref value=\"prefs\"/><state name=\"count\" value=\"1\"/>"
        + "<state name=\"who\" value=\"me\"/></gadget>"));
    LineContainers.appendLine(doc, XmlStringBuilder.createFromXmlString(
        "<img src=\"http://example.com/i.png\" width=\"10\" height=\"20\" alt=\"an image\"/>"
        + "after image<image attachment=\"att1\"><caption>the caption</caption></image>"
        + "<image attachment=\"att2\"></image><foo bar=\"baz\">x</foo>tail"));
    LineContainers.appendLine(doc, XmlStringBuilder.createFromXmlString(
        "<input name=\"in\">typed</input><input name=\"in2\" submit=\"sub\"></input>"
        + "<password name=\"pw\" value=\"secret\"/><check name=\"chk\" submit=\"s\" value=\"true\"/>"
        + "<label for=\"in\">Label</label><label></label>"
        + "<button name=\"btn\"><caption>Click</caption><events></events></button>"
        + "<button name=\"btn2\"><caption>Clicked</caption><events>"
        + "<click clicker=\"bob@example.com\" time=\"5\"/></events></button>"
        + "<radiogroup name=\"grp\" value=\"r1\"/><radio name=\"r1\" group=\"grp\"/>"
        + "<textarea name=\"ta\"><line></line>one<line></line>two</textarea>"
        + "<extension_installer manifest=\"http://example.com/m.xml\"/>"));
    // An inline reply in the first line and a reply below the blip.
    fx.author = BOB;
    fx.time = TIME + 1000;
    ObservableConversationBlip inline = root.addReplyThread(8).appendBlip();
    fx.append(inline, "inline reply");
    ObservableConversationBlip reply = root.addReplyThread().appendBlip();
    fx.append(reply, "first reply");
    ObservableConversationBlip reply2 = reply.getThread().appendBlip();
    fx.append(reply2, "second reply \u00E9 \u2603 snowman");
    ObservableConversationBlip nested = reply.addReplyThread().appendBlip();
    fx.append(nested, "nested");
    fx.author = ALICE;
    fx.time = TIME + 2000;
    ObservableConversationBlip second = fx.conv.getRootThread().appendBlip();
    fx.append(second, "second root blip");
    // Other documents.
    fx.wavelet.getDocument("tags").appendXml(
        XmlStringBuilder.createFromXmlString("<tag>one</tag><tag>two</tag>"));
    fx.wavelet.getDocument("r+key").appendXml(
        XmlStringBuilder.createFromXmlString("<data a=\"b\">value &amp; more</data>"));
    fx.wavelet.getDocument("r+too+many").appendXml(XmlStringBuilder.createText("ignored"));
    fx.wavelet.getDocument("roles").appendXml(XmlStringBuilder.createFromXmlString(
        "<assign address=\"bob@example.com\" role=\"READ_ONLY\"/>"));
    fx.wavelet.getDocument("attach+att1").appendXml(XmlStringBuilder.createFromXmlString(
        "<node key=\"mime_type\" value=\"image/png\"/>"
        + "<node key=\"attachment_url\" value=\"/attachment/att1?a=1&amp;b=2\"/>"));
    return fx;
  }

  /** A blip with form elements and a gadget, for element queries. */
  static Fx elements() {
    Fx fx = new Fx(ALICE);
    fx.wavelet.addParticipant(BOB);
    ObservableConversationBlip root = fx.conv.getRootThread().appendBlip();
    fx.append(root, "ab<input name=\"one\">x</input>cd<input name=\"two\">y</input>"
        + "<input name=\"three\">z</input>ef");
    LineContainers.appendLine(root.getContent(), XmlStringBuilder.createFromXmlString(
        "<gadget url=\"http://g.example/g.xml\" ifr=\"ifr1\"><title value=\"t\"/>"
        + "<state name=\"count\" value=\"1\"/></gadget>end"));
    return fx;
  }

  static {
    BASES.put("simple", new Builder() { public Fx build() { return simple(); } });
    BASES.put("rich", new Builder() { public Fx build() { return rich(); } });
    BASES.put("elements", new Builder() { public Fx build() { return elements(); } });
  }

  static String rootBlipId(Fx fx) {
    return fx.conv.getRootThread().getFirstBlip().getId();
  }

  // ---- Converters and the API view ----

  static void allBlips(Conversation conv, List<ConversationBlip> result) {
    List<ConversationThread> threads = Lists.newLinkedList();
    threads.add(conv.getRootThread());
    while (!threads.isEmpty()) {
      ConversationThread thread = threads.remove(0);
      for (ConversationBlip blip : thread.getBlips()) {
        result.add(blip);
        for (ConversationThread reply : blip.getReplyThreads()) {
          threads.add(reply);
        }
      }
    }
  }

  static void converters(String name) {
    Fx fx = BASES.get(name).build();
    fx.dump(name);
    List<ConversationBlip> blips = new ArrayList<ConversationBlip>();
    allBlips(fx.conv, blips);
    for (EventDataConverter converter : new EventDataConverter[] {
        new EventDataConverterV21(), new EventDataConverterV22()}) {
      String tag = converter instanceof EventDataConverterV22 ? "V22" : "V21";
      EventMessageBundle bundle = new EventMessageBundle("robot@example.com", "");
      line("V", tag, "W", GSON.toJson(converter.toWaveletData(fx.wavelet, fx.conv, bundle)));
      for (ConversationBlip blip : blips) {
        line("V", tag, "B", blip.getId(),
            GSON.toJson(converter.toBlipData(blip, fx.wavelet, bundle)));
      }
      line("V", tag, "T", GSON.toJson(bundle.getThreads(), GsonFactory.THREAD_MAP_TYPE));
    }
    for (ConversationBlip blip : blips) {
      Document doc = blip.getContent();
      ApiView view = new ApiView(doc, fx.wavelet);
      String contents = view.apiContents();
      line("A", blip.getId(), contents);
      StringBuilder xml = new StringBuilder();
      for (int i = 0; i <= contents.length() + 1; i++) {
        xml.append(view.transformToXmlOffset(i)).append(' ');
      }
      line("AX", blip.getId(), xml.toString().trim());
      StringBuilder text = new StringBuilder();
      for (int i = 0; i <= doc.size(); i++) {
        text.append(view.transformToTextOffset(i)).append(' ');
      }
      line("AT", blip.getId(), text.toString().trim());
    }
  }

  // ---- Operation services ----

  /** OperationContextImpl, reduced to one wavelet that is already open. */
  static class Ctx implements OperationContext {
    final Fx fx;
    final EventDataConverter converter = new EventDataConverterV22();
    final ConversationUtil conversationUtil = new ConversationUtil(ids("svc"));
    final Map<String, String> tempBlipIdMap = Maps.newHashMap();
    final Map<String, JsonRpcResponse> responses = Maps.newHashMap();
    ObservableConversationView view;

    Ctx(Fx fx) {
      this.fx = fx;
    }

    @Override public boolean isBound() {
      return true;
    }
    @Override public boolean hasResponse(String operationId) {
      return responses.containsKey(operationId);
    }
    @Override public void constructResponse(OperationRequest operation,
        Map<ParamsProperty, Object> data) {
      responses.put(operation.getId(), JsonRpcResponse.result(operation.getId(), data));
    }
    @Override public void constructErrorResponse(OperationRequest operation, String message) {
      responses.put(operation.getId(), JsonRpcResponse.error(operation.getId(), message));
    }
    @Override public void processEvent(OperationRequest operation, Event event)
        throws InvalidRequestException {
      if (event.getType() == EventType.OPERATION_ERROR) {
        constructErrorResponse(operation, OperationErrorEvent.as(event).getMessage());
        return;
      }
      try {
        constructResponse(operation, EventSerializer.extractPropertiesToParamsPropertyMap(event));
      } catch (EventSerializationException e) {
        throw new InvalidRequestException("Unable to serialize events", operation);
      }
    }
    @Override public void putWavelet(WaveId waveId, WaveletId waveletId, RobotWaveletData w) {
      throw new UnsupportedOperationException();
    }
    @Override public OpBasedWavelet openWavelet(WaveId waveId, WaveletId waveletId,
        ParticipantId participant) throws InvalidRequestException {
      if (!waveId.equals(WAVE) || !waveletId.equals(WAVELET)) {
        throw new InvalidRequestException("Wavelet couldn't be retrieved");
      }
      fx.author = participant;
      return fx.wavelet;
    }
    @Override public OpBasedWavelet openWavelet(OperationRequest operation,
        ParticipantId participant) throws InvalidRequestException {
      try {
        return openWavelet(
            ApiIdSerializer.instance().deserialiseWaveId(operation.getWaveId()),
            ApiIdSerializer.instance().deserialiseWaveletId(operation.getWaveletId()),
            participant);
      } catch (org.waveprotocol.wave.model.id.InvalidIdException e) {
        throw new InvalidRequestException("Invalid id", operation, e);
      }
    }
    @Override public ObservableConversationView openConversation(WaveId waveId,
        WaveletId waveletId, ParticipantId participant) throws InvalidRequestException {
      if (view == null) {
        view = conversationUtil.buildConversation(openWavelet(waveId, waveletId, participant));
      }
      return view;
    }
    @Override public ObservableConversationView openConversation(OperationRequest operation,
        ParticipantId participant) throws InvalidRequestException {
      openWavelet(operation, participant);
      return openConversation(WAVE, WAVELET, participant);
    }
    @Override public void putBlip(String blipId, ConversationBlip newBlip) {
      if (blipId.startsWith(TEMP_ID_MARKER)) {
        tempBlipIdMap.put(blipId, newBlip.getId());
      }
    }
    @Override public ConversationBlip getBlip(Conversation conversation, String blipId)
        throws InvalidRequestException {
      String actual = blipId.startsWith(TEMP_ID_MARKER) ? tempBlipIdMap.get(blipId) : blipId;
      ConversationBlip blip = conversation.getBlip(actual);
      if (blip == null) {
        throw new InvalidRequestException(
            "Blip with id " + blipId + " does not exist or has been deleted");
      }
      return blip;
    }
    @Override public EventDataConverter getConverter() {
      return converter;
    }
    @Override public ConversationUtil getConversationUtil() {
      return conversationUtil;
    }
    @Override public ImmutableSet<WaveletId> getVisibleWaveletIds(OperationRequest operation,
        ParticipantId participant) {
      throw new UnsupportedOperationException();
    }
    @Override public CommittedWaveletSnapshot getWaveletSnapshot(WaveletName waveletName,
        ParticipantId participant) {
      throw new UnsupportedOperationException();
    }
    @Override public void getDeltas(WaveletName waveletName, ParticipantId participant,
        HashedVersion fromVersion, HashedVersion toVersion,
        Receiver<TransformedWaveletDelta> receiver) {
      throw new UnsupportedOperationException();
    }
  }

  static OperationService serviceFor(OperationType type) {
    switch (type) {
      case DOCUMENT_MODIFY:
        return DocumentModifyService.create();
      case WAVELET_SET_TITLE:
        return WaveletSetTitleService.create();
      case WAVELET_ADD_PARTICIPANT_NEWSYNTAX:
      case WAVELET_REMOVE_PARTICIPANT_NEWSYNTAX:
        return ParticipantServices.create();
      case ROBOT_FETCH_WAVE:
        return FetchWaveService.create();
      default:
        return BlipOperationServices.create();
    }
  }

  /** Runs operations, in one context, on a fresh copy of a base state. */
  static void svc(String name, String base, ParticipantId actor, OperationRequest... operations) {
    Fx fx = BASES.get(base).build();
    Ctx ctx = new Ctx(fx);
    fx.time = TIME + 5000;
    for (OperationRequest operation : operations) {
      fx.captured.clear();
      line("SVC", name, base, actor.getAddress(), GSON.toJson(operation));
      String end;
      try {
        serviceFor(OperationType.fromMethodName(operation.getMethod()))
            .execute(operation, ctx, actor);
        JsonRpcResponse response = ctx.responses.get(operation.getId());
        end = response == null ? "NORESP" : "RESP\t" + GSON.toJson(response);
      } catch (InvalidRequestException e) {
        end = "IRE\t" + e.getMessage();
      } catch (RuntimeException e) {
        end = "EXC\t" + e.getClass().getSimpleName();
        if (System.getenv("GOLDEN_DEBUG") != null) e.printStackTrace();
      }
      for (WaveletOperation op : fx.captured) {
        opLine(op);
      }
      String[] parts = end.split("\t", 2);
      if (parts.length == 2) {
        line(parts[0], parts[1]);
      } else {
        line(parts[0]);
      }
    }
  }

  static int opCounter = 0;

  static OperationRequest op(OperationType type, String blipId, Parameter... params) {
    return new OperationRequest(type.method(), "op" + (++opCounter), WAVE_S, WAVELET_S, blipId,
        params);
  }

  static Parameter p(ParamsProperty key, Object value) {
    return Parameter.of(key, value);
  }

  static DocumentModifyAction action(ModifyHow how, String... values) {
    return new DocumentModifyAction(how, values.length == 0 ? null : Arrays.asList(values), null,
        null, null, false);
  }

  static DocumentModifyAction annotation(ModifyHow how, String key, String... values) {
    return new DocumentModifyAction(how, values.length == 0 ? null : Arrays.asList(values), key,
        null, null, false);
  }

  static DocumentModifyAction elementsAction(ModifyHow how, Element... elements) {
    return new DocumentModifyAction(how, null, null, Arrays.asList(elements), null, false);
  }

  static BlipData blipData(String tempId, String content) {
    return new BlipData(WAVE_S, WAVELET_S, tempId, content);
  }

  static Map<String, String> map(String... pairs) {
    Map<String, String> map = new LinkedHashMap<String, String>();
    for (int i = 0; i < pairs.length; i += 2) {
      map.put(pairs[i], pairs[i + 1]);
    }
    return map;
  }

  static void services() {
    String b = "b+baseA";
    // document.modify: text.
    svc("insert-index", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.INSERT, "XY")),
        p(ParamsProperty.INDEX, 3)));
    svc("insert-at-zero", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.INSERT, "first")),
        p(ParamsProperty.INDEX, 0)));
    svc("insert-multiline", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.INSERT, "one\ntwo\n")),
        p(ParamsProperty.INDEX, 6)));
    svc("insert-before-line", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.INSERT, "!")),
        p(ParamsProperty.INDEX, 12)));
    svc("insert-after-range", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.INSERT_AFTER, " appended")),
        p(ParamsProperty.RANGE, new Range(1, 6))));
    svc("insert-after-end", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.INSERT_AFTER, " at the end"))));
    svc("insert-beyond-end", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.INSERT, "far")),
        p(ParamsProperty.INDEX, 100)));
    svc("insert-annotated", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, new DocumentModifyAction(ModifyHow.INSERT,
            Arrays.asList("bold"), null, null,
            BundledAnnotation.listOf("style/fontWeight", "bold", "k", "v"), false)),
        p(ParamsProperty.INDEX, 7)));
    svc("replace-range", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.REPLACE, "Goodbye")),
        p(ParamsProperty.RANGE, new Range(1, 6))));
    svc("replace-across-lines", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.REPLACE, "X")),
        p(ParamsProperty.RANGE, new Range(9, 19))));
    svc("replace-all", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.REPLACE, "all new\ncontent"))));
    svc("delete-range", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.DELETE)),
        p(ParamsProperty.RANGE, new Range(3, 15))));
    svc("delete-from-zero", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.DELETE)),
        p(ParamsProperty.RANGE, new Range(0, 4))));
    svc("delete-all", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.DELETE))));
    svc("delete-empty-range", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.DELETE)),
        p(ParamsProperty.RANGE, new Range(4, 4))));
    svc("delete-index", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.DELETE)),
        p(ParamsProperty.INDEX, 12)));
    svc("annotate-range", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, annotation(ModifyHow.ANNOTATE, "style/color", "red")),
        p(ParamsProperty.RANGE, new Range(1, 6))));
    svc("annotate-all", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, annotation(ModifyHow.ANNOTATE, "k", "v"))));
    svc("annotate-out-of-bounds", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, annotation(ModifyHow.ANNOTATE, "k", "v")),
        p(ParamsProperty.RANGE, new Range(1, 600))));
    svc("annotate-then-clear", "simple", ALICE,
        op(OperationType.DOCUMENT_MODIFY, b,
            p(ParamsProperty.MODIFY_ACTION, annotation(ModifyHow.ANNOTATE, "k", "v")),
            p(ParamsProperty.RANGE, new Range(1, 10))),
        op(OperationType.DOCUMENT_MODIFY, b,
            p(ParamsProperty.MODIFY_ACTION, annotation(ModifyHow.CLEAR_ANNOTATION, "k")),
            p(ParamsProperty.RANGE, new Range(3, 6))),
        op(OperationType.DOCUMENT_MODIFY, b,
            p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.INSERT, "in")),
            p(ParamsProperty.INDEX, 2)),
        op(OperationType.DOCUMENT_MODIFY, b,
            p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.DELETE)),
            p(ParamsProperty.RANGE, new Range(2, 9))));
    svc("two-selectors", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.DELETE)),
        p(ParamsProperty.RANGE, new Range(1, 2)), p(ParamsProperty.INDEX, 1)));
    svc("no-action", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b));
    svc("no-such-blip", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, "b+nope",
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.DELETE))));
    // document.modify: elements.
    svc("query-annotate", "elements", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, annotation(ModifyHow.ANNOTATE, "k", "a", "b")),
        p(ParamsProperty.MODIFY_QUERY, new DocumentModifyQuery(ElementType.INPUT,
            new HashMap<String, String>(), -1))));
    svc("query-max", "elements", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.INSERT, "<", "<<")),
        p(ParamsProperty.MODIFY_QUERY, new DocumentModifyQuery(ElementType.INPUT,
            new HashMap<String, String>(), 2))));
    svc("query-restricted", "elements", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.INSERT_AFTER, ">")),
        p(ParamsProperty.MODIFY_QUERY, new DocumentModifyQuery(ElementType.INPUT,
            map("name", "two"), -1))));
    svc("query-delete", "elements", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.DELETE)),
        p(ParamsProperty.MODIFY_QUERY, new DocumentModifyQuery(ElementType.INPUT,
            new HashMap<String, String>(), -1))));
    svc("query-replace", "elements", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.REPLACE, "[input]")),
        p(ParamsProperty.MODIFY_QUERY, new DocumentModifyQuery(ElementType.INPUT,
            new HashMap<String, String>(), -1))));
    svc("query-line", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.INSERT_AFTER, "* ")),
        p(ParamsProperty.MODIFY_QUERY, new DocumentModifyQuery(ElementType.LINE,
            new HashMap<String, String>(), -1))));
    Gadget gadget = new Gadget("http://g.example/new.xml");
    gadget.setAuthor("alice@example.com");
    gadget.setProperty("score", "7");
    svc("insert-gadget", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, elementsAction(ModifyHow.INSERT, gadget)),
        p(ParamsProperty.INDEX, 3)));
    svc("insert-form-elements", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, elementsAction(ModifyHow.INSERT,
            new FormElement(ElementType.INPUT, "name", "default", "value"))),
        p(ParamsProperty.INDEX, 3)),
        op(OperationType.DOCUMENT_MODIFY, b,
            p(ParamsProperty.MODIFY_ACTION, elementsAction(ModifyHow.INSERT_AFTER,
                new FormElement(ElementType.BUTTON, "btn", "Click me", "Click me"))),
            p(ParamsProperty.INDEX, 3)),
        op(OperationType.DOCUMENT_MODIFY, b,
            p(ParamsProperty.MODIFY_ACTION, elementsAction(ModifyHow.INSERT,
                new FormElement(ElementType.TEXTAREA, "ta", "", "two\nlines"),
                new FormElement(ElementType.CHECK, "chk", "d", "true"),
                new FormElement(ElementType.LABEL, "in", "", "A label"),
                new FormElement(ElementType.PASSWORD, "pw", "", "secret"),
                new FormElement(ElementType.RADIO_BUTTON_GROUP, "grp", "", ""),
                new FormElement(ElementType.RADIO_BUTTON, "r1", "", "grp"))),
            p(ParamsProperty.MODIFY_QUERY, new DocumentModifyQuery(ElementType.LINE,
                new HashMap<String, String>(), -1))));
    svc("insert-image", "simple", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, elementsAction(ModifyHow.INSERT,
            new Image("http://example.com/i.png", 1, 2, "caption"))),
        p(ParamsProperty.INDEX, 3)));
    Gadget update = new Gadget("http://g.example/updated.xml");
    update.setProperty("count", "2");
    update.setProperty("fresh", "new");
    update.setProperty("title", "New title");
    update.setProperty("thumbnail", "thumb");
    svc("update-gadget", "elements", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, elementsAction(ModifyHow.UPDATE_ELEMENT, update)),
        p(ParamsProperty.MODIFY_QUERY, new DocumentModifyQuery(ElementType.GADGET,
            map("url", "http://g.example/g.xml"), -1))));
    svc("update-non-gadget", "elements", ALICE, op(OperationType.DOCUMENT_MODIFY, b,
        p(ParamsProperty.MODIFY_ACTION, elementsAction(ModifyHow.UPDATE_ELEMENT,
            new FormElement(ElementType.INPUT, "one", "", "changed"))),
        p(ParamsProperty.MODIFY_QUERY, new DocumentModifyQuery(ElementType.INPUT,
            new HashMap<String, String>(), -1))));
    // Blips.
    svc("append-blip", "simple", BOB, op(OperationType.WAVELET_APPEND_BLIP, null,
        p(ParamsProperty.BLIP_DATA, blipData("TBD_1", "\nappended blip"))));
    svc("append-blip-plain-id", "simple", BOB, op(OperationType.WAVELET_APPEND_BLIP, null,
        p(ParamsProperty.BLIP_DATA, blipData("whatever", "no newline\tand\ncontrol"))));
    svc("continue-thread", "rich", BOB, op(OperationType.BLIP_CONTINUE_THREAD, "b+baseC",
        p(ParamsProperty.BLIP_DATA, blipData("TBD_1", "\ncontinued"))));
    svc("create-child", "rich", BOB, op(OperationType.BLIP_CREATE_CHILD, "b+baseC",
        p(ParamsProperty.BLIP_DATA, blipData("TBD_1", "\nchild"))));
    svc("chain-with-temp-ids", "simple", BOB,
        op(OperationType.BLIP_CREATE_CHILD, b,
            p(ParamsProperty.BLIP_DATA, blipData("TBD_1", "\nchild"))),
        op(OperationType.BLIP_CONTINUE_THREAD, "TBD_1",
            p(ParamsProperty.BLIP_DATA, blipData("TBD_2", "\nnext"))),
        op(OperationType.DOCUMENT_MODIFY, "TBD_2",
            p(ParamsProperty.MODIFY_ACTION, action(ModifyHow.INSERT_AFTER, " more"))),
        op(OperationType.BLIP_DELETE, "TBD_1"),
        op(OperationType.BLIP_DELETE, "TBD_1"),
        op(OperationType.BLIP_DELETE, "TBD_2"));
    svc("append-inline-blip", "simple", BOB, op(OperationType.DOCUMENT_APPEND_INLINE_BLIP, b,
        p(ParamsProperty.BLIP_DATA, blipData("TBD_1", "\ninline at end"))));
    svc("insert-inline-blip", "simple", BOB, op(OperationType.DOCUMENT_INSERT_INLINE_BLIP, b,
        p(ParamsProperty.BLIP_DATA, blipData("TBD_1", "\ninline")),
        p(ParamsProperty.INDEX, 5)));
    svc("insert-inline-blip-zero", "simple", BOB, op(OperationType.DOCUMENT_INSERT_INLINE_BLIP, b,
        p(ParamsProperty.BLIP_DATA, blipData("TBD_1", "\ninline")),
        p(ParamsProperty.INDEX, 0)));
    svc("insert-inline-blip-after-element", "elements", BOB,
        op(OperationType.DOCUMENT_INSERT_INLINE_BLIP_AFTER_ELEMENT, b,
            p(ParamsProperty.BLIP_DATA, blipData("TBD_1", "\ninline")),
            p(ParamsProperty.ELEMENT,
                new FormElement(ElementType.INPUT, "two", "", "y"))),
        op(OperationType.DOCUMENT_INSERT_INLINE_BLIP_AFTER_ELEMENT, b,
            p(ParamsProperty.BLIP_DATA, blipData("TBD_2", "\ninline")),
            p(ParamsProperty.ELEMENT, new Gadget("http://g.example/g.xml"))),
        op(OperationType.DOCUMENT_INSERT_INLINE_BLIP_AFTER_ELEMENT, b,
            p(ParamsProperty.BLIP_DATA, blipData("TBD_3", "\ninline")),
            p(ParamsProperty.ELEMENT, new Gadget("http://g.example/none.xml"))));
    svc("append-markup", "simple", BOB, op(OperationType.DOCUMENT_APPEND_MARKUP, b,
        p(ParamsProperty.CONTENT, "some <b>markup</b> &amp; text")));
    svc("delete-blip", "rich", ALICE, op(OperationType.BLIP_DELETE, "b+baseC"));
    svc("delete-inline-blip", "rich", ALICE, op(OperationType.BLIP_DELETE, "b+baseB"));
    svc("delete-root-blip", "rich", ALICE, op(OperationType.BLIP_DELETE, "b+baseA"));
    // Wavelet.
    svc("set-title", "simple", ALICE, op(OperationType.WAVELET_SET_TITLE, null,
        p(ParamsProperty.WAVELET_TITLE, "New title")));
    svc("set-title-again", "rich", ALICE, op(OperationType.WAVELET_SET_TITLE, null,
        p(ParamsProperty.WAVELET_TITLE, "Another")));
    svc("participants", "simple", ALICE,
        op(OperationType.WAVELET_ADD_PARTICIPANT_NEWSYNTAX, null,
            p(ParamsProperty.PARTICIPANT_ID, "carol@example.com")),
        op(OperationType.WAVELET_ADD_PARTICIPANT_NEWSYNTAX, null,
            p(ParamsProperty.PARTICIPANT_ID, "carol@example.com")),
        op(OperationType.WAVELET_REMOVE_PARTICIPANT_NEWSYNTAX, null,
            p(ParamsProperty.PARTICIPANT_ID, "bob@example.com")),
        op(OperationType.WAVELET_REMOVE_PARTICIPANT_NEWSYNTAX, null,
            p(ParamsProperty.PARTICIPANT_ID, "bob@example.com")),
        op(OperationType.WAVELET_ADD_PARTICIPANT_NEWSYNTAX, null,
            p(ParamsProperty.PARTICIPANT_ID, "not an address")),
        op(OperationType.WAVELET_ADD_PARTICIPANT_NEWSYNTAX, null));
    svc("non-participant-acts", "simple", ROBOT, op(OperationType.WAVELET_APPEND_BLIP, null,
        p(ParamsProperty.BLIP_DATA, blipData("TBD_1", "\nby an outsider"))));
    svc("fetch-wave", "rich", ALICE, op(OperationType.ROBOT_FETCH_WAVE, null,
        p(ParamsProperty.MESSAGE, "a message")));
    svc("fetch-after-change", "simple", BOB,
        op(OperationType.BLIP_CREATE_CHILD, b,
            p(ParamsProperty.BLIP_DATA, blipData("TBD_1", "\nchild"))),
        op(OperationType.ROBOT_FETCH_WAVE, null));
  }

  // ---- Events ----

  interface Change {
    void run(Fx fx) throws Exception;
  }

  static Map<EventType, Capability> capabilities(Object... spec) {
    Map<EventType, Capability> result = new LinkedHashMap<EventType, Capability>();
    for (int i = 0; i < spec.length; i += 2) {
      EventType type = (EventType) spec[i];
      @SuppressWarnings("unchecked")
      List<Context> contexts = (List<Context>) spec[i + 1];
      result.put(type, contexts == null ? new Capability(type) : new Capability(type, contexts));
    }
    return result;
  }

  static final List<Context> ALL = Arrays.asList(Context.ALL);
  static final List<Context> SELF = Arrays.asList(Context.SELF);

  static Map<EventType, Capability> everything(List<Context> contexts) {
    List<Object> spec = new ArrayList<Object>();
    for (EventType type : EventType.values()) {
      if (type != EventType.UNKNOWN) {
        spec.add(type);
        spec.add(contexts);
      }
    }
    return capabilities(spec.toArray());
  }

  static String describe(Map<EventType, Capability> capabilities) {
    StringBuilder b = new StringBuilder();
    for (Capability c : capabilities.values()) {
      b.append(c.getEventType().name());
      for (Context context : c.getContexts()) {
        b.append(':').append(context.name());
      }
      b.append(' ');
    }
    return b.toString().trim();
  }

  /**
   * Applies changes to a base state, each change one delta by its author, and
   * generates the events of those deltas for a robot.
   */
  static void events(String name, String base, String robot, ProtocolVersion version,
      Map<EventType, Capability> capabilities, Object... authorsAndChanges) throws Exception {
    Fx fx = BASES.get(base).build();
    fx.reopen("ev");
    fx.captured.clear();
    List<TransformedWaveletDelta> deltas = new ArrayList<TransformedWaveletDelta>();
    List<List<WaveletOperation>> deltaOps = new ArrayList<List<WaveletOperation>>();
    for (int i = 0; i < authorsAndChanges.length; i += 2) {
      fx.author = (ParticipantId) authorsAndChanges[i];
      fx.time = TIME + 10000 + 1000 * i;
      ((Change) authorsAndChanges[i + 1]).run(fx);
      List<WaveletOperation> ops = new ArrayList<WaveletOperation>(fx.captured);
      fx.captured.clear();
      if (ops.isEmpty()) {
        continue;
      }
      deltas.add(TransformedWaveletDelta.cloneOperations(fx.author,
          HashedVersion.unsigned(fx.data.getVersion()), fx.time, ops));
      deltaOps.add(ops);
    }
    String stateName = "after-" + name;
    fx.dump(stateName);
    line("EV", name, stateName, robot, version.getVersionString(), describe(capabilities));
    for (int i = 0; i < deltas.size(); i++) {
      TransformedWaveletDelta delta = deltas.get(i);
      line("DELTA", delta.getAuthor().getAddress(), delta.getResultingVersion().getVersion(),
          delta.getApplicationTimestamp());
      for (WaveletOperation op : deltaOps.get(i)) {
        opLine(op);
      }
    }
    EventDataConverter converter = version == ProtocolVersion.V2_2
        ? new EventDataConverterV22() : new EventDataConverterV21();
    Gson gson = GSON;
    if (version == ProtocolVersion.V2) {
      gson = ElementGsonAdaptorV2.registerTypeAdapters(new GsonFactory()).create();
    }
    try {
      WaveletAndDeltas wad = WaveletAndDeltas.create(fx.data, DeltaSequence.of(deltas));
      EventMessageBundle bundle = new EventGenerator(RobotName.fromAddress(robot),
          new ConversationUtil(ids("gen"))).generateEvents(wad, capabilities, converter);
      line("BUNDLE", gson.toJson(bundle));
    } catch (RuntimeException e) {
      line("EXC", e.getClass().getSimpleName());
      if (System.getenv("GOLDEN_DEBUG") != null) e.printStackTrace();
    }
  }

  static ObservableConversationBlip blip(Fx fx, String id) {
    return fx.conv.getBlip(id);
  }

  static void eventScenarios() throws Exception {
    final String b = "b+baseA";
    Change addRobot = new Change() {
      public void run(Fx fx) {
        fx.wavelet.addParticipant(ROBOT);
      }
    };
    Change reply = new Change() {
      public void run(Fx fx) {
        ObservableConversationBlip blip = blip(fx, b).addReplyThread().appendBlip();
        fx.append(blip, "a reply");
      }
    };
    Change edit = new Change() {
      public void run(Fx fx) {
        Document doc = blip(fx, b).getContent();
        doc.insertText(5, "edited ");
        doc.setAnnotation(4, 9, "style/color", "red");
        doc.deleteRange(12, 14);
      }
    };
    Change removeRobot = new Change() {
      public void run(Fx fx) {
        fx.wavelet.removeParticipant(ROBOT);
      }
    };
    events("self-added", "simple", "robot@example.com", ProtocolVersion.V2_2, everything(null),
        ALICE, addRobot);
    events("self-added-v21", "rich", "robot@example.com", ProtocolVersion.V2_1,
        everything(null), ALICE, addRobot);
    events("self-added-v2-all", "rich", "robot@example.com", ProtocolVersion.V2,
        everything(ALL), ALICE, addRobot);
    events("not-a-participant", "simple", "robot@example.com", ProtocolVersion.V2_2,
        everything(null), ALICE, reply, BOB, edit);
    events("added-then-changes", "simple", "robot@example.com", ProtocolVersion.V2_2,
        everything(null), BOB, edit, ALICE, addRobot, BOB, reply, BOB, edit, ALICE, edit);
    events("added-and-more-in-one-delta", "simple", "robot@example.com", ProtocolVersion.V2_2,
        everything(SELF), ALICE, new Change() {
          public void run(Fx fx) {
            blip(fx, b).getContent().insertText(5, "before ");
            fx.wavelet.addParticipant(ROBOT);
            fx.wavelet.addParticipant(new ParticipantId("carol@example.com"));
            blip(fx, b).getContent().insertText(5, "after ");
            fx.wavelet.removeParticipant(BOB);
          }
        });
    events("only-some-capabilities", "simple", "robot@example.com", ProtocolVersion.V2_2,
        capabilities(EventType.WAVELET_BLIP_CREATED, null, EventType.DOCUMENT_CHANGED, SELF),
        ALICE, addRobot, BOB, reply, BOB, edit);
    events("own-deltas", "simple", "robot@example.com", ProtocolVersion.V2_2, everything(null),
        ALICE, addRobot, ROBOT, reply, ROBOT, edit, BOB, edit);
    events("added-by-itself", "simple", "robot@example.com", ProtocolVersion.V2_2,
        everything(null), ROBOT, addRobot, BOB, edit);
    events("removed", "simple", "robot@example.com", ProtocolVersion.V2_2, everything(null),
        ALICE, addRobot, BOB, edit, ALICE, removeRobot, BOB, reply);
    events("removed-and-added-again", "simple", "robot@example.com", ProtocolVersion.V2_2,
        everything(null), ALICE, addRobot, ALICE, new Change() {
          public void run(Fx fx) {
            fx.wavelet.removeParticipant(ROBOT);
            fx.wavelet.addParticipant(ROBOT);
            fx.wavelet.removeParticipant(BOB);
            fx.wavelet.addParticipant(BOB);
          }
        }, BOB, edit);
    events("proxy", "simple", "robot+proxy@example.com", ProtocolVersion.V2_2, everything(null),
        ALICE, new Change() {
          public void run(Fx fx) {
            fx.wavelet.addParticipant(new ParticipantId("robot+proxy@example.com"));
          }
        }, ROBOT, edit, BOB, reply);
    events("blips-created-and-removed", "rich", "robot@example.com", ProtocolVersion.V2_2,
        everything(SELF), ALICE, addRobot, BOB, new Change() {
          public void run(Fx fx) {
            ObservableConversationBlip first = blip(fx, "b+baseC").getThread().appendBlip();
            fx.append(first, "appended");
            ObservableConversationBlip inline = first.addReplyThread(3).appendBlip();
            fx.append(inline, "inline");
            blip(fx, "b+baseC").delete();
          }
        }, ALICE, new Change() {
          public void run(Fx fx) {
            blip(fx, "b+baseB").delete();
          }
        });
    events("contexts", "rich", "robot@example.com", ProtocolVersion.V2_2,
        capabilities(EventType.WAVELET_SELF_ADDED, Arrays.asList(Context.ROOT),
            EventType.DOCUMENT_CHANGED,
            Arrays.asList(Context.PARENT, Context.CHILDREN, Context.SIBLINGS)),
        ALICE, addRobot, BOB, new Change() {
          public void run(Fx fx) {
            blip(fx, "b+baseC").getContent().insertText(4, "!");
          }
        });
    events("contexts-v21", "rich", "robot@example.com", ProtocolVersion.V2_1,
        capabilities(EventType.WAVELET_SELF_ADDED, Arrays.asList(Context.ROOT),
            EventType.DOCUMENT_CHANGED,
            Arrays.asList(Context.PARENT, Context.CHILDREN, Context.SIBLINGS)),
        ALICE, addRobot, BOB, new Change() {
          public void run(Fx fx) {
            blip(fx, "b+baseC").getContent().insertText(4, "!");
            blip(fx, "b+baseD").getContent().insertText(4, "?");
          }
        });
    events("annotations", "simple", "robot@example.com", ProtocolVersion.V2_2,
        capabilities(EventType.ANNOTATED_TEXT_CHANGED, SELF), ALICE, addRobot, BOB,
        new Change() {
          public void run(Fx fx) {
            Document doc = blip(fx, b).getContent();
            doc.setAnnotation(4, 9, "style/color", "red");
            doc.setAnnotation(6, 12, "style/color", null);
            doc.setAnnotation(5, 7, "k", "v");
            doc.deleteRange(5, 8);
            doc.insertText(5, "new");
          }
        });
    events("button-and-gadget", "elements", "robot@example.com", ProtocolVersion.V2_2,
        everything(SELF), ALICE, new Change() {
          public void run(Fx fx) {
            fx.wavelet.addParticipant(ROBOT);
            LineContainers.appendLine(blip(fx, b).getContent(),
                XmlStringBuilder.createFromXmlString(
                    "<button name=\"go\"><caption>Go</caption><events></events></button>"));
          }
        }, BOB, new Change() {
          public void run(Fx fx) {
            Document doc = blip(fx, b).getContent();
            org.waveprotocol.wave.model.document.Doc.E events = null;
            org.waveprotocol.wave.model.document.Doc.E state = null;
            for (org.waveprotocol.wave.model.document.Doc.E el
                : org.waveprotocol.wave.model.document.util.DocIterate.deepElements(doc,
                    doc.getDocumentElement(), null)) {
              if (doc.getTagName(el).equals("events")) {
                events = el;
              }
              if (doc.getTagName(el).equals("state")) {
                state = el;
              }
            }
            doc.createChildElement(events, "click",
                attrs("clicker", "carol@example.com", "time", "12345"));
            doc.setElementAttribute(state, "value", "2");
          }
        });
    events("tags-and-data-documents", "rich", "robot@example.com", ProtocolVersion.V2_2,
        everything(SELF), ALICE, addRobot, BOB, new Change() {
          public void run(Fx fx) {
            fx.wavelet.getDocument("tags").appendXml(
                XmlStringBuilder.createFromXmlString("<tag>three</tag>"));
            fx.wavelet.getDocument("r+other").appendXml(XmlStringBuilder.createText("data"));
            TitleHelper.setExplicitTitle(blip(fx, b).getContent(), "Changed title");
          }
        });
  }

  static Map<String, String> attrs(String... pairs) {
    return map(pairs);
  }

  // ---- JSON of the API classes ----

  static void json(String kind, String name, Object value) {
    line("J", kind, name, GSON.toJson(value));
  }

  static void jsonScenarios() throws Exception {
    BlipData blip = new BlipData(WAVE_S, WAVELET_S, "TBD_blip", "content <&> \"quoted\"");
    blip.setCreator("alice@example.com");
    blip.addContributor("alice@example.com");
    blip.addAnnotation(new Annotation("style/color", "red", 1, 5));
    blip.addAnnotation(new Annotation("link/manual", "http://example.com/?a=b&c=d", 2, 3));
    blip.addElement(0, new Line());
    Line heading = new Line();
    heading.setLineType("h1");
    heading.setIndent("2");
    heading.setAlignment("c");
    heading.setDirection("r");
    blip.addElement(7, heading);
    blip.addElement(9, new Gadget("http://g.example/g.xml"));
    blip.addElement(10, new FormElement(ElementType.INPUT, "name", "default", "value"));
    blip.addElement(11, new FormElement(ElementType.CHECK));
    blip.addElement(12, new Image("http://example.com/i.png", 3, 4, "caption"));
    blip.addElement(13, new Image("attachmentId", "caption"));
    blip.addElement(14, new Element(ElementType.INLINE_BLIP, map("id", "b+x")));
    blip.addElement(15, new Attachment("the caption", new byte[] {1, 2, 3, (byte) 250}));
    Element withNull = new Element(ElementType.LABEL);
    withNull.setProperty("name", null);
    blip.addElement(16, withNull);
    blip.addChildBlipId("b+child");
    blip.setParentBlipId("b+parent");
    blip.setThreadId("thread");
    blip.setReplyThreadIds(Arrays.asList("t1", "t2"));
    blip.setLastModifiedTime(TIME);
    blip.setVersion(42);

    WaveletData wavelet = new WaveletData(WAVE_S, WAVELET_S, "b+root",
        new BlipThread("", -1, Arrays.asList("b+root", "b+2"), null));
    wavelet.setCreator("alice@example.com");
    wavelet.setCreationTime(TIME);
    wavelet.setLastModifiedTime(TIME + 5);
    wavelet.setVersion(77);
    wavelet.setTitle("A title");
    wavelet.addParticipant("alice@example.com");
    wavelet.addParticipant("bob@example.com");
    wavelet.setParticipantRole("bob@example.com", "READ_ONLY");
    wavelet.setDataDocument("key", "<x>value</x>");
    wavelet.addTag("tag1");
    wavelet.addTag("tag2");

    List<OperationRequest> requests = new ArrayList<OperationRequest>();
    requests.add(new OperationRequest("robot.notify", "op1",
        p(ParamsProperty.CAPABILITIES_HASH, "hash"), p(ParamsProperty.PROTOCOL_VERSION, "0.22")));
    requests.add(new OperationRequest("robot.createWavelet", "op2",
        p(ParamsProperty.WAVELET_DATA, wavelet), p(ParamsProperty.MESSAGE, "msg"),
        p(ParamsProperty.PROXYING_FOR, "proxy")));
    requests.add(new OperationRequest("wavelet.appendBlip", "op3", WAVE_S, WAVELET_S,
        p(ParamsProperty.BLIP_DATA, blip)));
    requests.add(new OperationRequest("wavelet.appendBlip", "op3b", WAVE_S, WAVELET_S,
        p(ParamsProperty.BLIP_DATA, new BlipData())));
    requests.add(new OperationRequest("wavelet.appendBlip", "op3c", WAVE_S, WAVELET_S,
        p(ParamsProperty.BLIP_DATA, new BlipData(WAVE_S, WAVELET_S, "TBD_1", null))));
    requests.add(new OperationRequest("document.modify", "op4", WAVE_S, WAVELET_S, "b+1",
        p(ParamsProperty.MODIFY_ACTION, new DocumentModifyAction(ModifyHow.REPLACE,
            Arrays.asList("a", null, "c"), "key", Arrays.<Element>asList(new Line(), null,
                new Gadget("http://g.example/")),
            BundledAnnotation.listOf("k1", "v1", "k2", "v2"), true)),
        p(ParamsProperty.MODIFY_QUERY, new DocumentModifyQuery(ElementType.GADGET,
            map("url", "http://g.example/"), 3))));
    requests.add(new OperationRequest("document.modify", "op5", WAVE_S, WAVELET_S, "b+1",
        p(ParamsProperty.MODIFY_ACTION, new DocumentModifyAction()),
        p(ParamsProperty.MODIFY_QUERY, new DocumentModifyQuery("needle", -1)),
        p(ParamsProperty.RANGE, new Range(3, 9)), p(ParamsProperty.INDEX, 5)));
    for (ModifyHow how : ModifyHow.values()) {
      requests.add(new OperationRequest("document.modify", "how-" + how.name(), WAVE_S,
          WAVELET_S, "b+1", p(ParamsProperty.MODIFY_ACTION, action(how))));
    }
    requests.add(new OperationRequest("document.insertInlineBlipAfterElement", "op6", WAVE_S,
        WAVELET_S, "b+1", p(ParamsProperty.BLIP_DATA, blip),
        p(ParamsProperty.ELEMENT, new FormElement(ElementType.BUTTON, "b", "d", "v"))));
    requests.add(new OperationRequest("robot.fetchWave", "op7", WAVE_S, WAVELET_S,
        p(ParamsProperty.RETURN_WAVELET_IDS, true)));
    requests.add(new OperationRequest("robot.search", "op8", p(ParamsProperty.QUERY, "in:inbox"),
        p(ParamsProperty.INDEX, 10), p(ParamsProperty.NUM_RESULTS, 20)));
    requests.add(new OperationRequest("robot.fetchProfiles", "op9",
        p(ParamsProperty.FETCH_PROFILES_REQUEST, new FetchProfilesRequest(
            Arrays.asList("alice@example.com", "bob@example.com"), "en"))));
    requests.add(new OperationRequest("robot.fetchProfiles", "op9b",
        p(ParamsProperty.FETCH_PROFILES_REQUEST, new FetchProfilesRequest())));
    requests.add(new OperationRequest("robot.exportDeltas", "op10", WAVE_S, WAVELET_S,
        p(ParamsProperty.FROM_VERSION, new byte[] {0, 1, 2, (byte) 255}),
        p(ParamsProperty.TO_VERSION, new byte[] {}),
        p(ParamsProperty.RAW_DELTAS, Arrays.asList(new byte[] {5, 6}, new byte[] {7}))));
    requests.add(new OperationRequest("robot.importAttachment", "op11", WAVE_S, WAVELET_S,
        p(ParamsProperty.ATTACHMENT_ID, "example.com/att"),
        p(ParamsProperty.ATTACHMENT_DATA, new RawAttachmentData("file.png",
            "alice@example.com", new byte[] {9, 8, 7}))));
    requests.add(new OperationRequest("robot.folderAction", "op12", WAVE_S, WAVELET_S, "b+1",
        p(ParamsProperty.MODIFY_HOW, "markAsRead")));
    requests.add(new OperationRequest("wavelet.setTitle", "op13", WAVE_S, WAVELET_S,
        p(ParamsProperty.WAVELET_TITLE, "tïtle ☃")));
    requests.add(new OperationRequest("document.setAnnotation", "op14", WAVE_S, WAVELET_S,
        "b+1", p(ParamsProperty.ANNOTATION, new Annotation("k", "v", 1, 2))));
    json("REQS", "requests", requests);
    line("J", "REQS", "requests-namespaced",
        new GsonFactory().create("wave").toJson(requests.subList(0, 3)));
    // What Gson makes of JSON written by others: read and written again.
    String foreign = "[{\"id\":7,\"method\":\"document.modify\",\"params\":{\"waveId\":"
        + "\"example.com!w+wave\",\"blipId\":null,\"index\":\"3\",\"unknown\":{\"x\":1},"
        + "\"modifyAction\":{\"modifyHow\":\"INSERT\",\"values\":[\"v\"],\"extra\":1},"
        + "\"blipData\":{\"blipId\":\"b\",\"elements\":{\"3\":{\"type\":\"line\","
        + "\"properties\":{\"indent\":2}}},\"version\":5.0},"
        + "\"range\":{\"start\":1},\"waveletData\":{\"participants\":[\"a@b.c\"]}}}]";
    List<OperationRequest> parsed = GSON.fromJson(foreign, GsonFactory.OPERATION_REQUEST_LIST_TYPE);
    line("J", "FOREIGN", "foreign", foreign);
    json("REQS", "foreign-read", parsed);

    Map<String, BlipData> blips = new LinkedHashMap<String, BlipData>();
    blips.put("b+1", blip);
    blips.put("b+2", new BlipData());
    Map<String, BlipThread> threads = new LinkedHashMap<String, BlipThread>();
    threads.put("t1", new BlipThread("t1", 5, Arrays.asList("b+2"), null));
    threads.put("t2", new BlipThread("t2", -1, new ArrayList<String>(), null));

    List<JsonRpcResponse> responses = new ArrayList<JsonRpcResponse>();
    responses.add(JsonRpcResponse.error("op1", "Something <went> wrong"));
    responses.add(JsonRpcResponse.result("op2", new HashMap<ParamsProperty, Object>()));
    Map<ParamsProperty, Object> data = new HashMap<ParamsProperty, Object>();
    data.put(ParamsProperty.BLIP_ID, "b+root");
    data.put(ParamsProperty.NEW_BLIP_ID, "b+new");
    responses.add(JsonRpcResponse.result("op3", data));
    data = new HashMap<ParamsProperty, Object>();
    data.put(ParamsProperty.BLIP_ID, "b+root");
    data.put(ParamsProperty.MESSAGE, null);
    data.put(ParamsProperty.WAVELET_DATA, wavelet);
    data.put(ParamsProperty.BLIPS, blips);
    data.put(ParamsProperty.THREADS, threads);
    responses.add(JsonRpcResponse.result("op4", data));
    data = new HashMap<ParamsProperty, Object>();
    data.put(ParamsProperty.PARTICIPANTS_ADDED, Arrays.asList("carol@example.com"));
    data.put(ParamsProperty.PARTICIPANTS_REMOVED, new ArrayList<String>());
    data.put(ParamsProperty.BLIP_ID, "");
    responses.add(JsonRpcResponse.result("op5", data));
    SearchResult search = new SearchResult("in:inbox");
    search.addDigest(new SearchResult.Digest("title", "snippet", WAVE_S,
        Arrays.asList("alice@example.com", "bob@example.com"), TIME, TIME - 10, 2, 5));
    search.addDigest(new SearchResult.Digest("", "", "example.com/w+2",
        new ArrayList<String>(), 0, 0, 0, 0));
    data = new HashMap<ParamsProperty, Object>();
    data.put(ParamsProperty.SEARCH_RESULTS, search);
    responses.add(JsonRpcResponse.result("op6", data));
    data = new HashMap<ParamsProperty, Object>();
    data.put(ParamsProperty.FETCH_PROFILES_RESULT, new FetchProfilesResult(Arrays.asList(
        new ParticipantProfile("alice@example.com", "Alice", "/img", "/profile"),
        new ParticipantProfile("Bob", "/img", ""))));
    responses.add(JsonRpcResponse.result("op7", data));
    data = new HashMap<ParamsProperty, Object>();
    data.put(ParamsProperty.WAVELET_IDS, ImmutableSet.of(WAVELET,
        WaveletId.of("example.com", "user+alice@example.com")));
    responses.add(JsonRpcResponse.result("op8", data));
    data = new HashMap<ParamsProperty, Object>();
    data.put(ParamsProperty.RAW_DELTAS, Arrays.asList(new byte[] {1, 2, 3}, new byte[] {}));
    data.put(ParamsProperty.TARGET_VERSION, new byte[] {(byte) 200, 100, 0});
    responses.add(JsonRpcResponse.result("op9", data));
    data = new HashMap<ParamsProperty, Object>();
    data.put(ParamsProperty.RAW_SNAPSHOT, "{\"1\":\"x\"}");
    data.put(ParamsProperty.IMPORTED_FROM_VERSION, 12345678901L);
    data.put(ParamsProperty.ATTACHMENT_DATA,
        new RawAttachmentData("f", "alice@example.com", new byte[] {1}));
    responses.add(JsonRpcResponse.result("op10", data));
    json("RESPS", "responses", responses);

    EventMessageBundle bundle = new EventMessageBundle("robot@example.com", "http://rpc/");
    bundle.setProxyingFor("proxy");
    bundle.setWaveletData(wavelet);
    bundle.setBlipData(blips);
    bundle.setThreads(threads);
    List<String> none = new ArrayList<String>();
    bundle.addEvent(new WaveletBlipCreatedEvent(null, null, "a@b.c", 1L, "b+root", "b+new"));
    bundle.addEvent(new WaveletBlipRemovedEvent(null, null, "a@b.c", 2L, "b+root", "b+old"));
    bundle.addEvent(new WaveletParticipantsChangedEvent(null, null, "a@b.c", 3L, "b+root",
        Arrays.asList("x@b.c"), Arrays.asList("y@b.c", "z@b.c")));
    bundle.addEvent(new WaveletSelfAddedEvent(null, null, "a@b.c", 4L, "b+root"));
    bundle.addEvent(new WaveletSelfRemovedEvent(null, null, "a@b.c", 5L, "b+root"));
    bundle.addEvent(new WaveletTitleChangedEvent(null, null, "a@b.c", 6L, "b+root", "title"));
    bundle.addEvent(new WaveletCreatedEvent(null, null, "a@b.c", 7L, "b+root", "msg", WAVE_S,
        WAVELET_S));
    bundle.addEvent(new WaveletFetchedEvent(null, null, "a@b.c", 8L, "msg", "b+root", wavelet,
        blips, threads));
    bundle.addEvent(new WaveletTagsChangedEvent(null, null, "a@b.c", 9L, "b+root"));
    bundle.addEvent(new BlipContributorsChangedEvent(null, null, "a@b.c", 10L, "b+1",
        Arrays.asList("x@b.c"), none));
    bundle.addEvent(new BlipSubmittedEvent(null, null, "a@b.c", 11L, "b+1"));
    bundle.addEvent(new DocumentChangedEvent(null, null, "a@b.c", 12L, "b+1"));
    bundle.addEvent(new FormButtonClickedEvent(null, null, "a@b.c", 13L, "b+1", "button"));
    bundle.addEvent(new GadgetStateChangedEvent(null, null, "a@b.c", 14L, "b+1", 3,
        map("key", "old")));
    bundle.addEvent(new AnnotatedTextChangedEvent(null, null, "a@b.c", 15L, "b+1", "name",
        "value"));
    bundle.addEvent(new AnnotatedTextChangedEvent(null, null, "a@b.c", 16L, "b+1", "name",
        null));
    bundle.addEvent(new OperationErrorEvent(null, null, "a@b.c", 17L, "op1", "failed"));
    json("BUNDLE", "bundle", bundle);
    line("J", "BUNDLE02", "bundle-0.2",
        ElementGsonAdaptorV2.registerTypeAdapters(new GsonFactory()).create().toJson(bundle));
    EventMessageBundle empty = new EventMessageBundle("robot@example.com", "");
    json("BUNDLE", "bundle-empty", empty);
  }

  public static void main(String[] args) throws Exception {
    out = new PrintStream(System.out, false, "US-ASCII");
    jsonScenarios();
    for (String base : BASES.keySet()) {
      converters(base);
    }
    services();
    eventScenarios();
    out.flush();
  }
}
