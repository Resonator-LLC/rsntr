// Generates wave_golden.txt from the original Apache Wave Java implementation,
// for the differential tests in src/golden_tests.rs.
//
// The protobuf messages are built with the Java protobuf runtime
// (DynamicMessage) from descriptors parsed out of the reference .proto files
// by the small parser below; no protoc-generated classes are used, so the
// field-setting logic of CoreWaveletOperationSerializer, SnapshotSerializer,
// ProtoDeltaStoreDataSerializer and AppliedDeltaUtil.buildAppliedDelta is
// mirrored here rather than executed. Ids, hashed versions, operations,
// transforms and wavelet data are the reference classes themselves.
//
// To regenerate (from the workspace root, with a JDK, a protobuf-java jar and
// a Guava jar):
//
//   W=reference/apache-wave/wave/src; T=$(mktemp -d)
//   mkdir -p $T/javax/annotation
//   echo 'package javax.annotation; public @interface Nullable {}' \
//       > $T/javax/annotation/Nullable.java
//   javac -nowarn -cp $PROTOBUF_JAR:$GUAVA_JAR -d $T/out \
//       -sourcepath $T:$W/main/java:$W/test/java \
//       crates/resonator-model/testdata/WaveGoldenGenerator.java
//   java -cp $T/out:$PROTOBUF_JAR:$GUAVA_JAR WaveGoldenGenerator $W/proto/proto \
//       > crates/resonator-model/testdata/wave_golden.txt
//
// Output format: one record per line, tab-separated, first field a tag;
// see line() for escaping. The records are described where they are written.

import com.google.protobuf.ByteString;
import com.google.protobuf.DescriptorProtos.DescriptorProto;
import com.google.protobuf.DescriptorProtos.EnumDescriptorProto;
import com.google.protobuf.DescriptorProtos.EnumValueDescriptorProto;
import com.google.protobuf.DescriptorProtos.FieldDescriptorProto;
import com.google.protobuf.DescriptorProtos.FileDescriptorProto;
import com.google.protobuf.Descriptors.Descriptor;
import com.google.protobuf.Descriptors.FieldDescriptor;
import com.google.protobuf.Descriptors.FileDescriptor;
import com.google.protobuf.DynamicMessage;

import org.waveprotocol.box.server.util.WaveletDataUtil;
import org.waveprotocol.wave.concurrencycontrol.common.DeltaPair;
import org.waveprotocol.wave.model.document.bootstrap.BootstrapDocument;
import org.waveprotocol.wave.model.document.operation.AnnotationBoundaryMap;
import org.waveprotocol.wave.model.document.operation.Attributes;
import org.waveprotocol.wave.model.document.operation.AttributesUpdate;
import org.waveprotocol.wave.model.document.operation.DocOp;
import org.waveprotocol.wave.model.document.operation.DocOpCursor;
import org.waveprotocol.wave.model.document.operation.impl.DocOpBuilder;
import org.waveprotocol.wave.model.document.operation.impl.DocOpUtil;
import org.waveprotocol.wave.model.id.IdGeneratorImpl;
import org.waveprotocol.wave.model.id.IdURIEncoderDecoder;
import org.waveprotocol.wave.model.id.LegacyIdSerialiser;
import org.waveprotocol.wave.model.id.ModernIdSerialiser;
import org.waveprotocol.wave.model.id.SimplePrefixEscaper;
import org.waveprotocol.wave.model.id.URIEncoderDecoder;
import org.waveprotocol.wave.model.id.WaveId;
import org.waveprotocol.wave.model.id.WaveIdentifiers;
import org.waveprotocol.wave.model.id.WaveletId;
import org.waveprotocol.wave.model.id.WaveletName;
import org.waveprotocol.wave.model.operation.OperationPair;
import org.waveprotocol.wave.model.operation.RemovedAuthorException;
import org.waveprotocol.wave.model.operation.TransformException;
import org.waveprotocol.wave.model.operation.wave.AddParticipant;
import org.waveprotocol.wave.model.operation.wave.BlipContentOperation;
import org.waveprotocol.wave.model.operation.wave.NoOp;
import org.waveprotocol.wave.model.operation.wave.RemoveParticipant;
import org.waveprotocol.wave.model.operation.wave.Transform;
import org.waveprotocol.wave.model.operation.wave.TransformedWaveletDelta;
import org.waveprotocol.wave.model.operation.wave.VersionUpdateOp;
import org.waveprotocol.wave.model.operation.wave.WaveletBlipOperation;
import org.waveprotocol.wave.model.operation.wave.WaveletDelta;
import org.waveprotocol.wave.model.operation.wave.WaveletOperation;
import org.waveprotocol.wave.model.operation.wave.WaveletOperationContext;
import org.waveprotocol.wave.model.testing.RandomDocOpGenerator;
import org.waveprotocol.wave.model.testing.RandomProviderImpl;
import org.waveprotocol.wave.model.version.HashedVersion;
import org.waveprotocol.wave.model.version.HashedVersionFactory;
import org.waveprotocol.wave.model.version.HashedVersionFactoryImpl;
import org.waveprotocol.wave.model.version.HashedVersionSerializer;
import org.waveprotocol.wave.model.wave.Constants;
import org.waveprotocol.wave.model.wave.ParticipantId;
import org.waveprotocol.wave.model.wave.ParticipantIdUtil;
import org.waveprotocol.wave.model.wave.data.ReadableBlipData;
import org.waveprotocol.wave.model.wave.data.ReadableWaveletData;
import org.waveprotocol.wave.model.wave.data.WaveletData;
import org.waveprotocol.wave.model.waveref.WaveRef;
import org.waveprotocol.wave.util.escapers.jvm.JavaUrlCodec;
import org.waveprotocol.wave.util.escapers.jvm.JavaWaverefEncoder;

import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeSet;

public class WaveGoldenGenerator {
  static PrintStream out;

  // Fields are tab-separated; characters outside printable ASCII and the
  // backslash are written as backslash-u escapes (four hex digits) of their
  // UTF-16 code units.
  static void line(Object... fields) {
    StringBuilder b = new StringBuilder();
    for (int i = 0; i < fields.length; i++) {
      if (i > 0) b.append('\t');
      String s = String.valueOf(fields[i]);
      for (int j = 0; j < s.length(); j++) {
        char c = s.charAt(j);
        if (c == '\t' || (c >= 0x20 && c < 0x7F && c != '\\')) {
          b.append(c);
        } else {
          b.append(String.format("\\u%04X", (int) c));
        }
      }
    }
    out.println(b);
  }

  static String hex(byte[] bytes) {
    StringBuilder b = new StringBuilder();
    for (byte x : bytes) b.append(String.format("%02x", x & 0xff));
    return b.toString();
  }

  // ---- Minimal .proto (proto2) parser producing descriptors. ----

  static List<String> tokenize(String src) {
    List<String> tokens = new ArrayList<String>();
    int i = 0;
    while (i < src.length()) {
      char c = src.charAt(i);
      if (Character.isWhitespace(c)) {
        i++;
      } else if (src.startsWith("//", i)) {
        while (i < src.length() && src.charAt(i) != '\n') i++;
      } else if (src.startsWith("/*", i)) {
        i = src.indexOf("*/", i) + 2;
      } else if (c == '"') {
        int j = src.indexOf('"', i + 1);
        tokens.add(src.substring(i, j + 1));
        i = j + 1;
      } else if (Character.isLetterOrDigit(c) || c == '_' || c == '.') {
        int j = i;
        while (j < src.length() && (Character.isLetterOrDigit(src.charAt(j))
            || src.charAt(j) == '_' || src.charAt(j) == '.')) j++;
        tokens.add(src.substring(i, j));
        i = j;
      } else {
        tokens.add(String.valueOf(c));
        i++;
      }
    }
    return tokens;
  }

  static final Map<String, FieldDescriptorProto.Type> SCALARS =
      new HashMap<String, FieldDescriptorProto.Type>();
  static {
    SCALARS.put("string", FieldDescriptorProto.Type.TYPE_STRING);
    SCALARS.put("bytes", FieldDescriptorProto.Type.TYPE_BYTES);
    SCALARS.put("bool", FieldDescriptorProto.Type.TYPE_BOOL);
    SCALARS.put("int32", FieldDescriptorProto.Type.TYPE_INT32);
    SCALARS.put("int64", FieldDescriptorProto.Type.TYPE_INT64);
  }

  static class Parser {
    final List<String> t;
    int p = 0;
    Parser(List<String> t) { this.t = t; }
    String next() { return t.get(p++); }
    void expect(String s) {
      String n = next();
      if (!n.equals(s)) throw new IllegalStateException("expected " + s + " got " + n);
    }
    void skipStatement() { while (!next().equals(";")) {} }
    void skipBlock() {
      expect("{");
      int depth = 1;
      while (depth > 0) {
        String n = next();
        if (n.equals("{")) depth++;
        if (n.equals("}")) depth--;
      }
    }

    FileDescriptorProto file(String name, String... deps) {
      FileDescriptorProto.Builder f = FileDescriptorProto.newBuilder().setName(name);
      for (String d : deps) f.addDependency(d);
      while (p < t.size()) {
        String n = next();
        if (n.equals("package")) {
          f.setPackage(next());
          expect(";");
        } else if (n.equals("message")) {
          f.addMessageType(message());
        } else if (n.equals("enum")) {
          f.addEnumType(enumType());
        } else if (n.equals("service")) {
          next();
          skipBlock();
        } else if (n.equals("syntax") || n.equals("import") || n.equals("option")) {
          skipStatement();
        } else if (!n.equals(";")) {
          throw new IllegalStateException("unexpected " + n);
        }
      }
      return f.build();
    }

    DescriptorProto message() {
      DescriptorProto.Builder m = DescriptorProto.newBuilder().setName(next());
      expect("{");
      while (true) {
        String n = next();
        if (n.equals("}")) break;
        if (n.equals("message")) {
          m.addNestedType(message());
        } else if (n.equals("enum")) {
          m.addEnumType(enumType());
        } else if (n.equals("option")) {
          skipStatement();
        } else if (n.equals(";")) {
          // empty statement
        } else {
          FieldDescriptorProto.Builder fld = FieldDescriptorProto.newBuilder();
          if (n.equals("required")) fld.setLabel(FieldDescriptorProto.Label.LABEL_REQUIRED);
          else if (n.equals("optional")) fld.setLabel(FieldDescriptorProto.Label.LABEL_OPTIONAL);
          else if (n.equals("repeated")) fld.setLabel(FieldDescriptorProto.Label.LABEL_REPEATED);
          else throw new IllegalStateException("unexpected label " + n);
          String type = next();
          if (SCALARS.containsKey(type)) fld.setType(SCALARS.get(type));
          else fld.setTypeName(type);
          fld.setName(next());
          expect("=");
          fld.setNumber(Integer.parseInt(next()));
          // Field options ((int52), default=false) do not affect the encoding.
          skipStatement();
          m.addField(fld);
        }
      }
      return m.build();
    }

    EnumDescriptorProto enumType() {
      EnumDescriptorProto.Builder e = EnumDescriptorProto.newBuilder().setName(next());
      expect("{");
      while (true) {
        String n = next();
        if (n.equals("}")) break;
        expect("=");
        e.addValue(EnumValueDescriptorProto.newBuilder().setName(n)
            .setNumber(Integer.parseInt(next())));
        expect(";");
      }
      return e.build();
    }
  }

  static FileDescriptor load(String root, String path, FileDescriptor... deps) throws Exception {
    String src = new String(Files.readAllBytes(Paths.get(root, path)), StandardCharsets.UTF_8);
    String[] depNames = new String[deps.length];
    for (int i = 0; i < deps.length; i++) depNames[i] = deps[i].getName();
    return FileDescriptor.buildFrom(new Parser(tokenize(src)).file(path, depNames), deps);
  }

  static FileDescriptor federation, deltaStore, clientRpc;

  static Descriptor fed(String name) {
    Descriptor d = federation.findMessageTypeByName(name);
    if (d == null) throw new IllegalStateException(name);
    return d;
  }

  static DynamicMessage.Builder b(Descriptor d) { return DynamicMessage.newBuilder(d); }

  static FieldDescriptor f(DynamicMessage.Builder b, String name) {
    FieldDescriptor fd = b.getDescriptorForType().findFieldByName(name);
    if (fd == null) throw new IllegalStateException(name);
    return fd;
  }

  static void set(DynamicMessage.Builder b, String name, Object v) { b.setField(f(b, name), v); }

  static void add(DynamicMessage.Builder b, String name, Object v) {
    b.addRepeatedField(f(b, name), v);
  }

  // ---- Mirror of CoreWaveletOperationSerializer. ----

  static DynamicMessage serialize(HashedVersion v) {
    DynamicMessage.Builder m = b(fed("ProtocolHashedVersion"));
    set(m, "version", v.getVersion());
    set(m, "history_hash", ByteString.copyFrom(v.getHistoryHash()));
    return m.build();
  }

  static DynamicMessage serialize(DocOp inputOp) {
    final Descriptor opD = fed("ProtocolDocumentOperation");
    final Descriptor compD = opD.findNestedTypeByName("Component");
    final DynamicMessage.Builder output = b(opD);
    inputOp.apply(new DocOpCursor() {
      private DynamicMessage.Builder c() { return b(compD); }
      private void emit(DynamicMessage.Builder c) { add(output, "component", c.build()); }
      @Override public void retain(int itemCount) {
        DynamicMessage.Builder c = c(); set(c, "retain_item_count", itemCount); emit(c);
      }
      @Override public void characters(String characters) {
        DynamicMessage.Builder c = c(); set(c, "characters", characters); emit(c);
      }
      @Override public void deleteCharacters(String characters) {
        DynamicMessage.Builder c = c(); set(c, "delete_characters", characters); emit(c);
      }
      @Override public void elementStart(String type, Attributes attributes) {
        DynamicMessage.Builder c = c(); set(c, "element_start", es(type, attributes)); emit(c);
      }
      @Override public void deleteElementStart(String type, Attributes attributes) {
        DynamicMessage.Builder c = c();
        set(c, "delete_element_start", es(type, attributes));
        emit(c);
      }
      private DynamicMessage kv(String key, String value) {
        DynamicMessage.Builder p = b(compD.findNestedTypeByName("KeyValuePair"));
        set(p, "key", key);
        set(p, "value", value);
        return p.build();
      }
      private DynamicMessage es(String type, Attributes attributes) {
        DynamicMessage.Builder e = b(compD.findNestedTypeByName("ElementStart"));
        set(e, "type", type);
        for (String name : attributes.keySet()) add(e, "attribute", kv(name, attributes.get(name)));
        return e.build();
      }
      @Override public void elementEnd() {
        DynamicMessage.Builder c = c(); set(c, "element_end", true); emit(c);
      }
      @Override public void deleteElementEnd() {
        DynamicMessage.Builder c = c(); set(c, "delete_element_end", true); emit(c);
      }
      @Override public void replaceAttributes(Attributes oldAttributes, Attributes newAttributes) {
        DynamicMessage.Builder r = b(compD.findNestedTypeByName("ReplaceAttributes"));
        if (oldAttributes.isEmpty() && newAttributes.isEmpty()) {
          set(r, "empty", true);
        } else {
          for (String name : oldAttributes.keySet()) {
            add(r, "old_attribute", kv(name, oldAttributes.get(name)));
          }
          for (String name : newAttributes.keySet()) {
            add(r, "new_attribute", kv(name, newAttributes.get(name)));
          }
        }
        DynamicMessage.Builder c = c(); set(c, "replace_attributes", r.build()); emit(c);
      }
      @Override public void updateAttributes(AttributesUpdate attributes) {
        DynamicMessage.Builder u = b(compD.findNestedTypeByName("UpdateAttributes"));
        if (attributes.changeSize() == 0) {
          set(u, "empty", true);
        } else {
          for (int i = 0; i < attributes.changeSize(); i++) {
            add(u, "attribute_update", kvu(attributes.getChangeKey(i), attributes.getOldValue(i),
                attributes.getNewValue(i)));
          }
        }
        DynamicMessage.Builder c = c(); set(c, "update_attributes", u.build()); emit(c);
      }
      @Override public void annotationBoundary(AnnotationBoundaryMap map) {
        DynamicMessage.Builder a = b(compD.findNestedTypeByName("AnnotationBoundary"));
        if (map.endSize() == 0 && map.changeSize() == 0) {
          set(a, "empty", true);
        } else {
          for (int i = 0; i < map.endSize(); i++) add(a, "end", map.getEndKey(i));
          for (int i = 0; i < map.changeSize(); i++) {
            add(a, "change", kvu(map.getChangeKey(i), map.getOldValue(i), map.getNewValue(i)));
          }
        }
        DynamicMessage.Builder c = c(); set(c, "annotation_boundary", a.build()); emit(c);
      }
      private DynamicMessage kvu(String key, String oldValue, String newValue) {
        DynamicMessage.Builder k = b(compD.findNestedTypeByName("KeyValueUpdate"));
        set(k, "key", key);
        if (oldValue != null) set(k, "old_value", oldValue);
        if (newValue != null) set(k, "new_value", newValue);
        return k.build();
      }
    });
    return output.build();
  }

  static DynamicMessage serialize(WaveletOperation waveletOp) {
    Descriptor d = fed("ProtocolWaveletOperation");
    DynamicMessage.Builder op = b(d);
    if (waveletOp instanceof NoOp) {
      set(op, "no_op", true);
    } else if (waveletOp instanceof AddParticipant) {
      set(op, "add_participant", ((AddParticipant) waveletOp).getParticipantId().getAddress());
    } else if (waveletOp instanceof RemoveParticipant) {
      set(op, "remove_participant",
          ((RemoveParticipant) waveletOp).getParticipantId().getAddress());
    } else if (waveletOp instanceof WaveletBlipOperation) {
      WaveletBlipOperation wbOp = (WaveletBlipOperation) waveletOp;
      DynamicMessage.Builder mutation = b(d.findNestedTypeByName("MutateDocument"));
      set(mutation, "document_id", wbOp.getBlipId());
      set(mutation, "document_operation",
          serialize(((BlipContentOperation) wbOp.getBlipOp()).getContentOp()));
      set(op, "mutate_document", mutation.build());
    } else {
      throw new IllegalArgumentException("Unsupported wavelet operation: " + waveletOp);
    }
    return op.build();
  }

  static DynamicMessage serialize(WaveletDelta delta) {
    DynamicMessage.Builder m = b(fed("ProtocolWaveletDelta"));
    for (WaveletOperation op : delta) add(m, "operation", serialize(op));
    set(m, "author", delta.getAuthor().getAddress());
    set(m, "hashed_version", serialize(delta.getTargetVersion()));
    return m.build();
  }

  static DynamicMessage serialize(TransformedWaveletDelta delta) {
    DynamicMessage.Builder m = b(fed("ProtocolWaveletDelta"));
    for (WaveletOperation op : delta) add(m, "operation", serialize(op));
    set(m, "author", delta.getAuthor().getAddress());
    set(m, "hashed_version", serialize(HashedVersion.unsigned(delta.getAppliedAtVersion())));
    return m.build();
  }

  // Mirror of ProtoDeltaStoreDataSerializer.serialize.
  static DynamicMessage serializeStored(TransformedWaveletDelta delta) {
    DynamicMessage.Builder m =
        b(deltaStore.findMessageTypeByName("ProtoTransformedWaveletDelta"));
    set(m, "author", delta.getAuthor().getAddress());
    set(m, "resulting_version", serialize(delta.getResultingVersion()));
    set(m, "application_timestamp", delta.getApplicationTimestamp());
    for (WaveletOperation op : delta) add(m, "operation", serialize(op));
    return m.build();
  }

  // Mirror of SnapshotSerializer.serializeWavelet, with documents in id order.
  static DynamicMessage serializeWavelet(ReadableWaveletData wavelet, HashedVersion version) {
    DynamicMessage.Builder m = b(clientRpc.findMessageTypeByName("WaveletSnapshot"));
    set(m, "wavelet_id", ModernIdSerialiser.INSTANCE.serialiseWaveletId(wavelet.getWaveletId()));
    for (ParticipantId p : wavelet.getParticipants()) add(m, "participant_id", p.toString());
    for (String id : new TreeSet<String>(wavelet.getDocumentIds())) {
      ReadableBlipData document = wavelet.getDocument(id);
      DynamicMessage.Builder d = b(clientRpc.findMessageTypeByName("DocumentSnapshot"));
      set(d, "document_id", document.getId());
      set(d, "document_operation", serialize(document.getContent().asOperation()));
      set(d, "author", document.getAuthor().getAddress());
      for (ParticipantId p : document.getContributors()) add(d, "contributor", p.getAddress());
      set(d, "last_modified_version", document.getLastModifiedVersion());
      set(d, "last_modified_time", document.getLastModifiedTime());
      add(m, "document", d.build());
    }
    set(m, "version", serialize(version));
    set(m, "last_modified_time", wavelet.getLastModifiedTime());
    set(m, "creator", wavelet.getCreator().getAddress());
    set(m, "creation_time", wavelet.getCreationTime());
    return m.build();
  }

  // Mirror of AppliedDeltaUtil.buildAppliedDelta.
  static DynamicMessage buildAppliedDelta(DynamicMessage signedDelta, HashedVersion appliedAt,
      int operationsApplied, long applicationTimestamp) {
    DynamicMessage.Builder m = b(fed("ProtocolAppliedWaveletDelta"));
    set(m, "signed_original_delta", signedDelta);
    set(m, "operations_applied", operationsApplied);
    set(m, "application_timestamp", applicationTimestamp);
    set(m, "hashed_version_applied_at", serialize(appliedAt));
    return m.build();
  }

  // ---- Id, participant and version cases. ----

  static final IdURIEncoderDecoder URI_CODEC = new IdURIEncoderDecoder(new JavaUrlCodec());
  static final URIEncoderDecoder STRING_CODEC = new URIEncoderDecoder(new JavaUrlCodec());
  static final HashedVersionFactory HASH_FACTORY = new HashedVersionFactoryImpl(URI_CODEC);

  static final String[] STRINGS = {
      "", "a", "abc", "example.com", "Example.com", "example.com.", ".example.com",
      "exa-mple.c-m", "-example.com", "example-.com", "example..com", "a.b.c.d", "1.2.3",
      "localhost", "ex_ample.com", "w+abc", "conv+root", "b+1~+2", "a~b", "a!b", "a/b", "a b",
      "a%b", "a:b", "id~", "~", "+", "*", "@", ".-_~+@*", "user+fred@example.com",
      "jörg", "tūdaliņ.lv", " ", "\u009F", "�", "﷐", "￯",
      "", "豈", "x𐀀y", "󿿽", "󰀀", "\u007F",
      "a\u0000b", "café+中文", "prof+jörg@tūdaliņ.lv",
  };

  static final String[] SERIALISED = {
      "example.com/id", "example.com/w+abc", "example.com!w+abc", "example.com!id",
      "example.com", "example.com/", "example.com//", "/id", "/example.com/id",
      "example.com/waveid/waveletid", "example.com/1/example.com/3/",
      "example.com/id/example.com/id", "example.com/waveid/example2.com/waveletid",
      "example.com/waveid/~/waveletid", "example.com/equal/example2.com/equal",
      "example.com/w+abc/~/conv+root", "example.com/w+abc/other.org/user+a@other.org",
      "Example.com/id", "example.com/some~2Fid", "example.com!some~2Fid", "example.com!a~+b",
      "example.com!a!b", "example.com!!b", "!id", "example.com!", "ex~ample.com!id",
      "example.com!i~~d+part", "example.com!id~", "", "/", "!", "a/b/c/d/e", "a/b/c/d",
      "example.com/prof+jörg@tūdaliņ.lv", "example.com/a b",
      "example.com/w+a/~/a b", "example.com/x𐀀y/~/conv+root",
      "a.com/id/~/id/", "a.com/id/a.com/id", "a.com/i!d", "a.com!i/d",
  };

  static final String[] URIS = {
      "wave://example.com/w+abc/conv+root", "wave://other.org/example.com!w+abc/user+a@other.org",
      "wave://example.com/w%2Babc/conv+root", "wave://example.com/a%20b/c+d",
      "wave://example.com/a%2fb/c%C3%A9", "wave://example.com/a%C3/c", "wave://example.com/a%zz/c",
      "wave://example.com/a%2/c", "wave://example.com/a/c%", "wave://example.com/a/b/c",
      "wave://example.com/a", "http://example.com/a/b", "wave://example.com/x!y!z/c",
      "wave://example.com//c", "wave://example.com/a%EF%BF%BDb/c", "wave://example.com/a~b/c",
      "wave://example.com/%7E%2Ba/c", "wave://ex~ample.com/a/c", "wave://example.com/a/b!c",
      "wave:///a/b", "wave://example.com/d!/c",
  };

  static final String[] ADDRESSES = {
      "a@example.com", "A@Example.COM", "@example.com", "a@", "@", "a", "", "a@b@c",
      "a@b.c@", "@@", "fred@wavesandbox.com", "jÖrg@example.com", "a@-example.com",
      "a@example", "a@ex ample.com", "a b@example.com", "unknown", "a@example.com.",
  };

  static final String[] WAVEREFS = {
      "example.com/w+abcd1234", "example.com/w+abcd1234/~/conv+root",
      "example.com/w+abcd1234/~/conv+root/b+12345", "example.com/w+abc/other.org/conv+root",
      "Example.com/w+abc", "example.com/w+abc/~", "example.com", "", "/", "//a/b",
      "example.com/w+abc/~/conv+root/b+1/extra", "example.com/w%2Babc", "example.com/w%20abc",
      "example.com/w abc", "example.com/w%2fabc", "example.com/w+abc/~/conv%2Broot/b%2B1",
      "example.com/w%zz", "example.com/caf%C3%A9", "example.com/w+abc/~//b", "example.com//x",
      "example.com/w+abc/EXAMPLE.com/conv+root", "example.com/a%C3", "ex%61mple.com/w+abc",
      "example.com/w+abc/%7E/conv+root",
  };

  interface Call { String run() throws Exception; }

  static String attempt(Call c) {
    try {
      return "OK\t" + c.run();
    } catch (Exception e) {
      return "ERR";
    } catch (AssertionError e) {
      return "ERR";
    }
  }

  static String parts(WaveletName n) {
    return n.waveId.getDomain() + "\t" + n.waveId.getId() + "\t" + n.waveletId.getDomain() + "\t"
        + n.waveletId.getId();
  }

  static void idCases() throws Exception {
    // IDV s valid; DOM start s valid
    for (String s : STRINGS) {
      line("IDV", s, WaveIdentifiers.isValidIdentifier(s));
      for (int start = 0; start <= Math.min(2, s.length()); start++) {
        line("DOM", start, s, WaveIdentifiers.isValidDomain(start, s));
      }
      // ENC s encoded; DEC s (OK decoded | ERR)
      line("ENC", s, STRING_CODEC.encode(s));
      final String fs = s;
      line("DEC", s, attempt(() -> STRING_CODEC.decode(fs)));
      // ESC s escaped (OK unescaped | ERR) hasEscapeCharacters isEscapedProperly('+')
      line("ESC", s, SimplePrefixEscaper.DEFAULT_ESCAPER.escape(s),
          attempt(() -> SimplePrefixEscaper.DEFAULT_ESCAPER.unescape(fs)));
      line("ESP", s, SimplePrefixEscaper.DEFAULT_ESCAPER.hasEscapeCharacters(s),
          SimplePrefixEscaper.DEFAULT_ESCAPER.isEscapedProperly('+', s));
      // SPL s (OK n tokens... | ERR): split on '+' with unescaping
      line("SPL", s, attempt(() -> {
        String[] t = SimplePrefixEscaper.DEFAULT_ESCAPER.split('+', fs);
        return t.length + "\t" + String.join("\t", t);
      }));
      // PTH s pathEncoded queryEncoded
      line("PTH", s, JavaWaverefEncoder.encodeToUriPathSegment(s),
          JavaWaverefEncoder.encodeToUriQueryString(s));
      for (String d : new String[] {"example.com", "ex!ample.com", ""}) {
        // IDS domain id of(OK|ERR) ofLegacy(OK|ERR)
        final String fd = d;
        line("IDS", d, s, attempt(() -> WaveId.of(fd, fs).getId()).substring(0, 2),
            attempt(() -> WaveletId.ofLegacy(fd, fs).getId()).substring(0, 2));
      }
    }
    // WMOD/WLEG/WDUAL/WLID s (OK domain id | ERR); NAME s (OK parts serialised uri hexV0 | ERR)
    for (final String s : SERIALISED) {
      line("WMOD", s, attempt(() -> {
        WaveId id = ModernIdSerialiser.INSTANCE.deserialiseWaveId(s);
        WaveletId wid = ModernIdSerialiser.INSTANCE.deserialiseWaveletId(s);
        if (!id.getId().equals(wid.getId())) throw new IllegalStateException();
        return id.getDomain() + "\t" + id.getId();
      }));
      line("WLEG", s, attempt(() -> {
        WaveId id = LegacyIdSerialiser.INSTANCE.deserialiseWaveId(s);
        return id.getDomain() + "\t" + id.getId();
      }));
      line("WDUAL", s, attempt(() -> {
        WaveId id = WaveId.checkedDeserialise(s);
        return id.getDomain() + "\t" + id.getId();
      }));
      line("WLID", s, attempt(() -> {
        WaveletId id = WaveletId.checkedDeserialise(s);
        return id.getDomain() + "\t" + id.getId() + "\t" + id.serialise();
      }));
      line("NAME", s, attempt(() -> {
        WaveletName n = ModernIdSerialiser.INSTANCE.deserialiseWaveletName(s);
        return parts(n) + "\t" + ModernIdSerialiser.INSTANCE.serialiseWaveletName(n) + "\t"
            + URI_CODEC.waveletNameToURI(n) + "\t"
            + hex(HASH_FACTORY.createVersionZero(n).getHistoryHash());
      }));
    }
    // URI s (OK parts | ERR)
    for (final String s : URIS) {
      line("URI", s, attempt(() -> parts(URI_CODEC.uriToWaveletName(s))));
    }
    // PID s (OK address name domain | ERR) unvalidatedAddress normalized domainAddress
    for (final String s : ADDRESSES) {
      line("PID", s, attempt(() -> {
        ParticipantId p = ParticipantId.of(s);
        return p.getAddress() + "\t" + p.getName() + "\t" + p.getDomain();
      }));
      line("PIN", s, new ParticipantId(s).getAddress(), ParticipantIdUtil.isNormalizedAddress(s),
          ParticipantIdUtil.isDomainAddress(s));
    }
    // PCMP a b signum(compareTo)
    for (String a : ADDRESSES) {
      for (String c : ADDRESSES) {
        line("PCMP", a, c,
            Integer.signum(new ParticipantId(a).compareTo(new ParticipantId(c))));
      }
    }
    // WREF s (OK waveDomain waveId [waveletDomain waveletId [docId]] | ERR), then for OK:
    // WREFE pathEncoded queryEncoded
    for (final String s : WAVEREFS) {
      final WaveRef[] ref = new WaveRef[1];
      line("WREF", s, attempt(() -> {
        ref[0] = null;
        WaveRef r = JavaWaverefEncoder.decodeWaveRefFromPath(s);
        ref[0] = r;
        String x = r.getWaveId().getDomain() + "\t" + r.getWaveId().getId();
        if (r.hasWaveletId()) {
          x += "\t" + r.getWaveletId().getDomain() + "\t" + r.getWaveletId().getId();
        }
        if (r.hasDocumentId()) x += "\t" + r.getDocumentId();
        return x;
      }));
      if (ref[0] != null) {
        line("WREFE", JavaWaverefEncoder.encodeToUriPathSegment(ref[0]),
            JavaWaverefEncoder.encodeToUriQueryString(ref[0]));
      }
    }
    // TOK n base64
    for (int n : new int[] {0, 1, 63, 64, 65, 127, 128, 4095, 4096, 262144, 16777216, 1073741823,
        1073741824, Integer.MAX_VALUE}) {
      line("TOK", n, IdGeneratorImpl.base64Encode(n));
    }
    // HVS version hexHash toString; HVC between consecutive entries: signum(compareTo)
    byte[][] hashes = {{}, {0}, {1}, {127}, {(byte) 128}, {(byte) 255}, {1, 2}, {1, (byte) 200},
        "AAA".getBytes(), "wave://example.com/w+abc/conv+root".getBytes(),
        {(byte) 0xfb, (byte) 0xff, (byte) 0xfe}};
    List<HashedVersion> versions = new ArrayList<HashedVersion>();
    for (long v : new long[] {0, 1, 10, Long.MAX_VALUE}) {
      for (byte[] h : hashes) versions.add(HashedVersion.of(v, h));
    }
    for (HashedVersion v : versions) {
      String s = HashedVersionSerializer.INSTANCE.toString(v);
      if (!s.equals(v.toString())) throw new IllegalStateException();
      if (!HashedVersionSerializer.INSTANCE.fromString(s).equals(v)) {
        throw new IllegalStateException();
      }
      line("HVS", v.getVersion(), hex(v.getHistoryHash()), s);
    }
    for (int i = 0; i < versions.size(); i += 3) {
      for (int j = 0; j < versions.size(); j += 5) {
        line("HVC", i, j, versions.get(i).compareTo(versions.get(j)));
      }
    }
  }

  // ---- Delta, hash chain and wavelet data scenarios. ----

  static final String[][] NAMES = {
      {"example.com", "w+abc", "example.com", "conv+root"},
      {"example.com", "w+abc", "other.org", "user+a@other.org"},
      {"a.b.c", "w+café", "a.b.c", "conv+中文"},
      {"example.com", "prof+jörg@tūdaliņ.lv", "example.com", "conv+x*y"},
      {"x.org", "w+x𐀀y", "y.org", "conv+~~a"},
  };
  static final String[] POOL =
      {"a@example.com", "b@example.com", "c@other.org", "@example.com"};
  static final String[] DOCS = {"b+1", "b+2", "attach+x", "conversation", "tr+1"};

  static WaveletOperationContext ctx(ParticipantId p) {
    return new WaveletOperationContext(p, Constants.NO_TIMESTAMP, 1);
  }

  static void dump(ReadableWaveletData w) {
    // W version hashedVersion lastModifiedTime creator creationTime participants(,)
    StringBuilder ps = new StringBuilder();
    for (ParticipantId p : w.getParticipants()) ps.append(p.getAddress()).append(',');
    line("W", w.getVersion(), w.getHashedVersion().getVersion() + ":"
        + hex(w.getHashedVersion().getHistoryHash()), w.getLastModifiedTime(),
        w.getCreator().getAddress(), w.getCreationTime(), ps);
    // WD id author contributors(,) lastModifiedTime lastModifiedVersion xml
    for (String id : new TreeSet<String>(w.getDocumentIds())) {
      ReadableBlipData d = w.getDocument(id);
      StringBuilder cs = new StringBuilder();
      for (ParticipantId p : d.getContributors()) cs.append(p.getAddress()).append(',');
      line("WD", id, d.getAuthor().getAddress(), cs, d.getLastModifiedTime(),
          d.getLastModifiedVersion(), DocOpUtil.toXmlString(d.getContent().asOperation()));
    }
  }

  static void failingDelta(WaveletData wavelet, ParticipantId author, HashedVersion v,
      List<WaveletOperation> ops) {
    TransformedWaveletDelta td = TransformedWaveletDelta.cloneOperations(author,
        HashedVersion.unsigned(v.getVersion() + ops.size()), 2000000L, ops);
    // F (OK | exception class), then the wavelet dump
    try {
      WaveletDataUtil.applyWaveletDelta(td, wavelet);
      line("F", "OK");
    } catch (Exception e) {
      line("F", e.getClass().getSimpleName());
    }
    dump(wavelet);
  }

  static void scenarios() throws Exception {
    RandomDocOpGenerator.Parameters p = new RandomDocOpGenerator.Parameters();
    for (int seed = 0; seed < 12; seed++) {
      RandomDocOpGenerator.RandomProvider r = RandomProviderImpl.ofSeed(seed);
      String[] n = NAMES[seed % NAMES.length];
      WaveletName name = WaveletName.of(n[0], n[1], n[2], n[3]);
      HashedVersion v = HASH_FACTORY.createVersionZero(name);
      // SCN seed hexV0
      line("SCN", seed, hex(v.getHistoryHash()));
      Map<String, BootstrapDocument> mirror = new HashMap<String, BootstrapDocument>();
      List<String> participants = new ArrayList<String>();
      WaveletData wavelet = null;
      for (int i = 0; i < 6; i++) {
        ParticipantId author = ParticipantId.ofUnsafe(POOL[r.nextInt(3)]);
        int nops = 1 + r.nextInt(3);
        List<WaveletOperation> ops = new ArrayList<WaveletOperation>();
        for (int k = 0; k < nops; k++) {
          int kind = r.nextInt(6);
          if (kind == 0) {
            String who = POOL[r.nextInt(4)];
            if (participants.remove(who)) {
              ops.add(new RemoveParticipant(ctx(author), ParticipantId.ofUnsafe(who)));
            } else {
              participants.add(who);
              ops.add(new AddParticipant(ctx(author), ParticipantId.ofUnsafe(who)));
            }
          } else if (kind == 1) {
            ops.add(new NoOp(ctx(author)));
          } else {
            String docId = DOCS[r.nextInt(5)];
            BootstrapDocument doc = mirror.get(docId);
            if (doc == null) {
              doc = new BootstrapDocument();
              mirror.put(docId, doc);
            }
            DocOp op = RandomDocOpGenerator.generate(r, p, doc);
            doc.consume(op);
            ops.add(new WaveletBlipOperation(docId, new BlipContentOperation(ctx(author), op)));
          }
        }
        long ts = 1000000L + i * 1000L;
        WaveletDelta delta = new WaveletDelta(author, v, ops);
        byte[] deltaBytes = serialize(delta).toByteArray();
        // D hex(ProtocolWaveletDelta)
        line("D", hex(deltaBytes));
        DynamicMessage.Builder signed = b(fed("ProtocolSignedDelta"));
        set(signed, "delta", ByteString.copyFrom(deltaBytes));
        if (i % 3 == 2) {
          Descriptor sigD = fed("ProtocolSignature");
          DynamicMessage.Builder sig = b(sigD);
          set(sig, "signature_bytes", ByteString.copyFrom(new byte[] {1, 2, 3}));
          set(sig, "signer_id", ByteString.copyFrom(new byte[] {9, 8}));
          set(sig, "signature_algorithm",
              sigD.findEnumTypeByName("SignatureAlgorithm").findValueByName("SHA1_RSA"));
          add(signed, "signature", sig.build());
        }
        byte[] appliedBytes = buildAppliedDelta(signed.build(), v, nops, ts).toByteArray();
        // A hex(ProtocolAppliedWaveletDelta)
        line("A", hex(appliedBytes));
        HashedVersion v2 = HASH_FACTORY.create(appliedBytes, v, nops);
        // V version hexHash
        line("V", v2.getVersion(), hex(v2.getHistoryHash()));
        TransformedWaveletDelta td = TransformedWaveletDelta.cloneOperations(v2, ts, delta);
        // T hex(ProtoTransformedWaveletDelta); U hex(ProtocolWaveletDelta of the transformed delta)
        line("T", hex(serializeStored(td).toByteArray()));
        line("U", hex(serialize(td).toByteArray()));
        if (i == 0) {
          wavelet = WaveletDataUtil.createEmptyWavelet(name, author, HashedVersion.unsigned(0), ts);
        }
        WaveletDataUtil.applyWaveletDelta(td, wavelet);
        dump(wavelet);
        // S hex(WaveletSnapshot)
        line("S", hex(serializeWavelet(wavelet, v2).toByteArray()));
        v = v2;
      }
      // Deltas whose last operation fails, to exercise the rollback.
      ParticipantId author = ParticipantId.ofUnsafe(POOL[0]);
      ParticipantId nobody = ParticipantId.ofUnsafe("nobody@nowhere.com");
      BootstrapDocument doc = mirror.get(DOCS[0]);
      if (doc == null) doc = new BootstrapDocument();
      DocOp op = RandomDocOpGenerator.generate(r, p, doc);
      failingDelta(wavelet, author, v, Arrays.<WaveletOperation>asList(
          new WaveletBlipOperation(DOCS[0], new BlipContentOperation(ctx(author), op)),
          new NoOp(ctx(author)),
          new RemoveParticipant(ctx(author), nobody)));
      op = RandomDocOpGenerator.generate(r, p, new BootstrapDocument());
      failingDelta(wavelet, author, v, Arrays.<WaveletOperation>asList(
          new WaveletBlipOperation("b+new", new BlipContentOperation(ctx(author), op)),
          new AddParticipant(ctx(author), nobody),
          new RemoveParticipant(ctx(author), nobody),
          new RemoveParticipant(ctx(author), nobody)));
      failingDelta(wavelet, author, v, Arrays.<WaveletOperation>asList(
          new WaveletBlipOperation("b+bad", new BlipContentOperation(ctx(author),
              new DocOpBuilder().retain(5).build()))));
    }
  }

  // ---- Transform scenarios. ----

  static String describe(WaveletOperation op) {
    if (op instanceof VersionUpdateOp) return "VU:" + op;
    return hex(serialize(op).toByteArray());
  }

  static String describe(List<WaveletOperation> ops) {
    StringBuilder b = new StringBuilder();
    for (WaveletOperation op : ops) b.append(describe(op)).append(',');
    return b.toString();
  }

  static WaveletOperation blip(ParticipantId who, String docId, DocOp op) {
    return new WaveletBlipOperation(docId, new BlipContentOperation(ctx(who), op));
  }

  static List<WaveletOperation> pool(ParticipantId who, RandomDocOpGenerator.RandomProvider r,
      RandomDocOpGenerator.Parameters p, BootstrapDocument base) {
    List<WaveletOperation> ops = new ArrayList<WaveletOperation>();
    ops.add(new AddParticipant(ctx(who), ParticipantId.ofUnsafe("x@example.com")));
    ops.add(new AddParticipant(ctx(who), ParticipantId.ofUnsafe("a@example.com")));
    ops.add(new RemoveParticipant(ctx(who), ParticipantId.ofUnsafe("a@example.com")));
    ops.add(new RemoveParticipant(ctx(who), ParticipantId.ofUnsafe("b@example.com")));
    ops.add(new RemoveParticipant(ctx(who), ParticipantId.ofUnsafe("x@example.com")));
    ops.add(new NoOp(ctx(who)));
    ops.add(blip(who, "b+1", RandomDocOpGenerator.generate(r, p, base)));
    ops.add(blip(who, "b+1", RandomDocOpGenerator.generate(r, p, base)));
    ops.add(blip(who, "b+2", RandomDocOpGenerator.generate(r, p, base)));
    return ops;
  }

  static void transforms() throws Exception {
    RandomDocOpGenerator.Parameters p = new RandomDocOpGenerator.Parameters();
    ParticipantId a = ParticipantId.ofUnsafe("a@example.com");
    ParticipantId bb = ParticipantId.ofUnsafe("b@example.com");
    for (int seed = 100; seed < 106; seed++) {
      RandomDocOpGenerator.RandomProvider r = RandomProviderImpl.ofSeed(seed);
      BootstrapDocument base = new BootstrapDocument();
      for (int i = 0; i < 3; i++) base.consume(RandomDocOpGenerator.generate(r, p, base));
      List<WaveletOperation> client = pool(a, r, p, base);
      List<WaveletOperation> server = pool(bb, r, p, base);
      // XS seed
      line("XS", seed);
      for (int i = 0; i < client.size(); i++) {
        for (int j = 0; j < server.size(); j++) {
          // X i j (OK client server | RA | TE)
          try {
            OperationPair<WaveletOperation> pair = Transform.transform(client.get(i), server.get(j));
            line("X", i, j, "OK", describe(pair.clientOp()), describe(pair.serverOp()));
          } catch (RemovedAuthorException e) {
            line("X", i, j, "RA");
          } catch (TransformException e) {
            line("X", i, j, "TE");
          }
        }
      }
      // Delta pairs: two doc ops in sequence on each side, plus participant ops.
      BootstrapDocument cdoc = new BootstrapDocument();
      cdoc.consume(base.asOperation());
      BootstrapDocument sdoc = new BootstrapDocument();
      sdoc.consume(base.asOperation());
      List<WaveletOperation> cd = new ArrayList<WaveletOperation>();
      List<WaveletOperation> sd = new ArrayList<WaveletOperation>();
      for (int i = 0; i < 2; i++) {
        DocOp c = RandomDocOpGenerator.generate(r, p, cdoc);
        cdoc.consume(c);
        cd.add(blip(a, "b+1", c));
        DocOp s = RandomDocOpGenerator.generate(r, p, sdoc);
        sdoc.consume(s);
        sd.add(blip(bb, "b+1", s));
      }
      cd.add(client.get(0));
      sd.add(server.get(0));
      sd.add(server.get(5));
      // DP clientOps(,) serverOps(,)
      DeltaPair pair = new DeltaPair(cd, sd).transform();
      line("DP", describe(pair.getClient()), describe(pair.getServer()));
      // Identical deltas by the same author nullify each other.
      List<WaveletOperation> same = new ArrayList<WaveletOperation>();
      for (WaveletOperation op : cd) {
        same.add(WaveletOperation.cloneOp(op, new WaveletOperationContext(a, 5L, 1)));
      }
      pair = new DeltaPair(cd, same).transform();
      line("DP", describe(pair.getClient()), describe(pair.getServer()));
    }
  }

  public static void main(String[] args) throws Exception {
    out = new PrintStream(System.out, false, "UTF-8");
    String root = args[0];
    federation = load(root, "org/waveprotocol/wave/federation/federation.protodevel");
    deltaStore = load(root, "org/waveprotocol/box/server/persistence/protos/delta-store.proto",
        federation);
    clientRpc = load(root, "org/waveprotocol/box/common/comms/waveclient-rpc.proto", federation);
    idCases();
    scenarios();
    transforms();
    out.flush();
  }
}
