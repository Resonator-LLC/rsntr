# Google Wave Federation Architecture Overview — transcript

- Source: https://www.youtube.com/watch?v=CRZbHpYhZrA
- Channel: Google for Developers · Aug 12, 2009 · 41:30
- Speaker: Soren Lassen (Google Wave Federation team)
- Transcript: YouTube auto-generated captions, unedited. They are noisy: words are garbled and some passages are missing.

## Introduction

0:15 eh right Thanks Dan for the intro Thanks
0:21 for your Thanks for coming Everybody Thanks for your Patience so eh It's a
0:27 great eh we quite excited in Google w Federation Engineering Team to have this
0:32 opportunity to to share our thoughts ands and our Questions with
0:38 you today and so most recently we've been very busy completing this open
0:46 source drop eh Can you hear me oke eh so most
0:53 recently we been very busy preparing this open source drop that we doing today eh so eh completing The Code and
1:01 packaging it and on the whole It's been a busy year
1:08 we the Concept of Federation was something that was Integral to the
1:14 Google Wave Concept from from very Inception but it took us a while to
1:21 before the the design started to gel and only in the beginning of the Year we we
1:27 got to the point where we could start implementing this and then we
1:33 accelerated the the announcement at Google IO in May we we
1:39 eh applied ourselves to eh get a demo ready That was part of the
1:47 Keynote Where Google way was announced and we managed to put out a very early draft
1:54 of the specification on the same day eh Which was very important to us
2:00 because weed to eh make that make it clear that this is central
2:08 to Vision for Google Wave eh The
2:15 Draft very early draft with some trepidation that we Put out something
2:21 that was that Unfinished but the the feedback we got was very positive and receptive and we based on
2:29 the feedback we were able to quickly improve The Draft and iterate on
2:34 it however We Found that As we were doing that that It's very laborious and slow going to
2:42 develop a spe as a as a way of developing Protocol so we we looked
2:50 ourselves and see How could we make better progress and How could we make the process more useful for everybody
2:56 and we eh decided to eh accelerate schedule for cutting code and
3:03 sharing code with with you so
3:09 eh That's what we doing today we eh we will keep iterating on Specs and
3:16 so on but we believe it's more productive that we alongside speak in
3:23 code so eh eh based on eh based on all the eh
3:29 positive feedback we decided to schedule Federation day for today and and plan an
3:36 open source release so we been working on agressive schedule we only realized
3:41 how agressive it was after set the and work towards it but
3:52 it's you might see engers
4:00 great exper as we as we worked this people all over the company froms have
4:07 been coming forward helping us and here in the last week quite a number of
4:15 people different sets of expertise have been helping this Come Together

## Open Source

4:26 eh eh structur my talk around the open
4:33 source release eh let see
4:42 so Yeah so the motivation should be be clear for where we com from so
4:47 eh what what are we releasing and what are we not releasing
4:52 eh eh What We are releasing break down four categories so
5:02 is part of our Production code eh so we our plan for Google Wave is
5:12 that the most of our code Production code that we use in our client our
5:17 servers we will open source but it will take us a while for a number of reasons
5:22 and we in to something out of the door Quick
5:27 we been focusing on things that we believe are the most useful for us and
5:33 for you and so in Production code What is the the most useful of All is the
5:41 operational transformation This is central to the Protocol It's and it's
5:46 central to the Protocol that everybody agree on the algorithm that's how that's
5:51 how this conflict resolution eh eh Protocol works so eh
6:00 while We are we still committed writing a full formal specification of this but
6:06 we think particularly useful to share the code that actually does
6:13 transformations so that we can agree on it and and it's we find
6:19 that It's more productive that we share this code early rather than sitting around trying to a good form
6:25 specification for it as first thing so be doing that in par
6:30 eh you can find now on on the Protocol
6:37 code along with operational transformation algorithm Comes the data
6:43 structur that goes with it so we also together with it comes the data
6:49 struct the operations themselves and the documents to which operations apply I'll
6:55 tell a little bit more about exactly what operation transformation and operation
7:03 then we This is in Wave Model eh package in w Protocol We have some
7:12 Protocol Google eh Protocol buffer definitions that we
7:19 use for a number of things we use them for internally Wire Protocol and Protocol
7:26 use in Systems and as you see later today when we talk more about
7:34 the xmpp Wire Protocol and Security aspects of the Protocol
7:42 sessions in the afternoon you see that the particulars about cation is
7:47 important for the hashing and signing that is part of our
7:53 security in the Protocol eh then eh thirdly We
8:01 have an end prototype that runs Wave server Wave client and
8:08 Federation Port eh so
8:14 we CR fed One eh and put it up in the
8:20 examples fed one directory as possibly One of multiple examples we we could
8:26 have in the future eh I will describe this but to say this
8:35 point it Turned out that It was the fastest way we could get something out
8:41 there That's useful was not to cut it from our Production code but to build
8:48 simplistic prototype Focus on Simplicity and
8:54 Clarity that we have been writing from from the ground for for this
9:01 release So it's the purp are two fold for illustration can see through code
9:09 how the Technology works and secondly this we want this to work
9:16 executable specification so that when we discuss
9:21 the Protocol that we can have this code to back up the
9:26 semantics of the Protocol
9:32 eh finally as together with this we the
9:38 exec spec we got in our hands informed Our Own understanding of the Protocol so
9:45 we been making a revision of the spe so that it matches this prototype As
9:52 we been doing this we clos some security gaps there some holes in in the earlier draft the
10:01 Protocol eh and some other gaps about parameters
10:06 and so on eh and then we as as I said many people have
10:12 Steed up to help us and Among others people with more understanding of xmpp than than the original Team so we have
10:19 refined our mapping to the messages to xmpp
10:25 eh this we will cover in the afternoon server xm
10:32 session i mostly now for the rest of the Talk
10:39 explain more about how operational transformation works and with emphasis
10:44 on the particular Flavor that Google Wave operational transformation has and
10:50 then I will give an architectural overview of the fed One prototype
11:00 so before I dive in let just many of you know but let's Just be clear on what
11:07 what is that w server w
11:13 service must provide in order to deliver Wave if you look w

## Wave Panel

11:21 c the central things
11:30 Here we have the we call The Wave panel This is where you see a wave this
11:35 particular case full of hyperlinks and Image attachments and gadgets but
11:43 eh It's eas to think think of the text portion as here in Here you can Type
11:50 characters and while you Type you can others can Type We
11:56 have collaborative Edi
12:03 key level conflicts are res you
12:09 have conflict resolution and real time
12:15 collab so when you run a w client you need to
12:20 connect to a wave server That's able to serve up What The Wave looks like
12:26 enter when you open it and to give you live updates of the
12:35 eh will give you eh eh the server must
12:40 give live updates to the to clients so that in real time you see the key
12:46 Strokes of others and the Edits of others as they are applied To The Wave and your Edits must in real time travel
12:55 to the server so that It can be Shared with other clients likewise and then Let
13:01 me eh mention here the
13:07 eh the search panel eh so in order to
13:12 open a wave in the first place and find waves that others invite you to you you need to be
13:20 able to see and search your waves so eh
13:26 there many other things to it as I mentioned Gadget atts
13:33 and playback to Consult the history of The Wave and on we
13:39 eh so in the prototype eh we
13:44 Chosen the simplest barebone implementation of a client on server so
13:50 the focus is on editing and on on an inbox you can see waves eh so
14:02 eh Let me just quickly Run Through What a wave look like many of you probably
14:08 already have seen diagrams of this but a wave as you see in The Wave
14:13 panel consists of eh These XML documents
14:19 eh and eh the different the way structured
14:24 is that you can have multiple XML documents that can Act as replies to
14:30 eh and what we call document in Wave
14:36 is a little bit more than an XML document We have These annotations turns out that when you
14:45 have when you have robots that help you put hyperlinks
14:52 and spelling suggestions and other things onto the w editing then
14:59 much eas
15:05 handle Con contributions of them IF stack them in annotations that are
15:11 sitting outs of the the tre structure of the
15:18 XML then One more participants mple parp can Wave and then
15:29 a wave itself is thre Conversation so the reply structure can be just
15:36 documents that refer to each other but you might you can also have private replies Where you have different sets of
15:43 participants on different portions of the different threats Within a
15:49 conversation This is Model by having multiple
15:55 WS parti and when we come to Federation It's the
16:02 wavelet the central Unit Of Federation this is what sharing the
16:11 Wave itself is nothing other than the w inside it in fact Wave is ID
16:20 and just a thing you constr by All That The

## Operations

16:26 Wave operations a wave I say a wavelet wavelet really Unit
16:35 Of the whole system w is constructed from
16:40 operations and These operations down the document level they are correspond to
16:48 text Edits so here an example of how operations
16:55 work the side see clients eh it starts
17:01 with a document with contents AB c d eh
17:06 the eh the user of this client then deles
17:12 the fourth character the eh Content of the document becomes abce
17:20 eh simultaneously this is operation app to document ches station
17:36 the server is talking with Cent that is simultaneous editing the
17:43 state from persec this centes Something Going On the server It would be some
17:48 other El what's going on the server is in
17:56 thise SIM on
18:02 that the state of document ches AC now
18:09 eh the big Benefit you get from operation
18:14 transformation is that on this client out here
18:20 eh the client can apply this Edit immediately soon
18:26 as user press key It shows up as the document Has Changed the state
18:33 accordingly while in the background the client synchronizes that State change
18:39 with the server eh in this case because there a simultaneous Edit by somebody
18:45 else Who Hit the server First The client never manages to drive the server State to
18:51 abce but it eh eh there an automatic conflict resolution So that the
19:00 server agree on how two
19:05 Edits are are put together so that they
19:12 converge on the same state in the end and so what happens is that the ser sees
19:18 that the client Wanted up here delete theth character something else happen in
19:24 between it then transforms the clients operation relative to the service operation in this
19:30 Cas What the meant was theth character becomes third character because delet
19:37 before so to delete the D is now delete the third character and you come to a the client runs
19:45 same operation transformation algorithm in
19:50 the and transforms the operation relative to operation in this case the
19:58 server and we
20:06 Con the user does and and by contin operational
20:12 transformation
20:55 conver Con eh so we are doing a couple of eh so the

## Transformation

21:07 whole what I just described is standard operation transformation that you will find in in the literature on
21:14 collaborative document ting and so eh what I described is often shown as
21:22 diagram you have a client operation server operation You have the transform against each other ser operation
21:28 transforms to transform Operation The Cent operation to transform Cent operation This is all good so the two
21:35 things that we done is that we put in support
21:41 for for XML rather than PL text which is useful of course
21:49 because we have a rich text editor I think it's not Unique I think
21:54 There are other Systems out there but eh eh
22:00 but it's Fairly new entry into operational
22:05 transformation and then there's an other Aspect done namely that our operations
22:12 compose so look at this example here where we have one server operation and two client
22:19 operations eh so normally you then take them One at a
22:25 time when you have These two client operations transform operation
22:37 second operation now what we have is operations comp Take
22:46 two arit consecutive operations and compose them Together into single operation And
22:53 Then if take the operation and
22:59 same had transform One by one against This is useful because
23:08 it allows efficient transforms against large when large number
23:16 operations This is Looking forward
23:23 to Disconnect for a while between Cent and a server
23:30 a lot could have happened on the server before the Cent comes back could happened on the Cent IF Cent Lets the
23:37 user Edit while he online offline I should So supposing there many client
23:45 operations and many operations then the without without this composition you end
23:52 up with multiplicative number of transformations you need to do
23:59 eh with composition if you first compose the
24:05 cation then and the ser operations then you end up with single transformation and even though the compos operations
24:12 may be Larger than time complexity
24:19 becomes Yeah we got num some between linear and n log in which is
24:25 dramatically better than multiplic

## Operation

24:31 so the way we are able to compose operations is that an operation is not
24:37 just a single Edit or a single insert we allow an operation to express an arbitrary
24:43 diff Between Two document States so an operation consists of a sequence of
24:50 components These components can be skips that Nothing happens to These eight
24:57 characters can this
25:02 point tag can insert some characters it can delete end
25:09 tag Skip does change delete characters
25:15 This is the format operation how we can Express arbitrary diff Across arbitrary
25:24 docum and then
25:29 operations These two operations basically sorted list of things to do
25:36 inserts and deletes eh eh and and then
25:41 composing two operations is comparable to merging to sort list
25:47 sipping them up eh and transformation is
25:53 works similarly you have These two order list that you then match up
26:00 line travion Of The
26:06 Two That's what I wanted to cover about the way Model and operation
26:14 transformation a lot more to when you see the code
26:19 there Wave views and w hold Together participant l and documents but eh eh so
26:31 eh eh The Code should be Hopefully straight forward and supported by
26:37 looking at the the fed one example we hope that you find it
26:44 informative and if and Will plenty of opportunity during the day
26:50 to in sessions later and inform during the day to ask questions about

## Prototype

27:02 Ok let me jump to the other point I to cover here in the talk and that's what
27:09 this looks like so here diagram
27:16 of of our prototype Across each side of the do
27:23 Line you have two different instances of F One so
27:28 fed one has a wave server eh in the Middle eh and it then connects
27:37 to eh Zero or more clients so we build a client that runs
27:45 as so I should say everything is Within Java so the here as opposed to our product Where you will
27:52 find the Wave client runs in the Browser here The Wave client is a
27:59 eh Line client that terminal
28:06 Cent using curs and text eh
28:13 shows inbox you can see all your waves ordered by when they were last modified eh and
28:22 then you can Select one the Wes opens we don't it works like
28:31 un This is again to find the simp thing that would illustrate a working
28:42 System of coding to to do more interesting collaborative editing inside it Which
28:49 we didn't put in at this point all the the way Model supports all this eh
29:02 Con tcb connection runs Google protoc bu
29:10 AC to the w server
29:15 front where it sents that
29:21 Connected and then inside the server in of deltas
29:29 when I use the word Delta and you hear that later today also just
29:34 means Con Series of operations so say for instance that client both added a
29:41 participant to and did appended line to a document These
29:48 two things can be these are two operations but they may be sent as one
29:53 Network packet to the server in which case We Roll them into Call Delta
30:00 eh eh Yeah so Delta is just sequence of operations eh but that's The Unit we use
30:08 for for for sharing the These deltas are the ones that are Shared between clients
30:16 that are The Unit Of storage The Unit Of sharing Across the Federation Port with
30:22 other servers

## Assigning hashing

30:28 Ja That eh now eh the w server also does
30:34 signing and hashing which is part of the security measures that are in the
30:39 Protocol to prevent spoofing eh I'm not gna detail how it works what
30:48 it does session in the afternoon about this but eh
30:56 It's w the Signature of every Delta every Delta signed and the Signature
31:03 sted in the w Store for Federation purposes So what happens is that the w
31:10 server is connecting to an xmpp
31:15 server so we Right Now we running an open fire xmpp server but the
31:23 eh using standard Protocol for acting as a component inside the xmpp
31:31 server so the w connects to the xmpp server and registers as Wave I think and
31:40 which means that if another Wave
31:46 server wants to share deltas with this one then over the xnp
31:54 Protocol to the XP server Then come in
31:59 For The Wave go the w server this alls two w servers on each
32:06 side of the Line Here we have think running two different
32:13 domains Let's say we have a.com here.com
32:20 on the left here on the IF IF
32:28 user here on a wavelet St at
32:34 incs participant Who is a user at acne.com
32:40 then the in cor Wave server will start eh
32:46 Broadcasting shouldn't say broadcast but start foring deltas on this wavelet to
32:55 the.com ac p server who will forward it to the w server over here who will then
33:02 forward it to the the one user That was
33:15 intended Yes I think that's what want say about this

## Indexing

33:23 diagram So let me just Run Through here
33:30 the the s yeah It's Worth mentioning
33:37 that it serves up an index it maintains an index of all the w in the system both
33:44 the w h locally and the one learns about from that are hosted remotely in this
33:52 case is.com users can w
33:59 WC host and can see the w that hosted on ins
34:05 and user was added to and.com by Federation eh so in this this
34:14 is one of the places where this prototype diverges from the actual Production system We have we in order to
34:21 to bu The simplest index Wave
34:28 in inbo Wave connect to Wave and populates
34:35 inbo and that's how we can get a inb so as as people add to waves they show
34:43 up eh so the Of concurrency Control
34:49 conflict resolution using operation transformation So this happens both between local users if there two users
34:55 the same at the same time or if there remote users
35:03 on the eh the the w St is in memory when all the
35:12 deltas are signed the and I explained how we connect to the outside World as a
35:20 by xmp component in xmpp server eh so one thing here Let me just return pict
35:28 one one important thing here Where also this di from this is simpler system that
35:35 we running in the actual Wave Cent so to make this as simple as possible we chose
35:41 in this prototype not to do operational transformation on the clients this Cent is not
35:47 optimistic so As I said it works as a unix talk Type Cent So you you typ Line
35:52 and Across to the server and reflected in your view of the St ech back the
35:59 server you Lear app but was just to save of
36:07 Code you can even use Cent ser Protocol We have here and
36:12 build an optimistic client so we call this a Thin client because it doesn't doot you can build
36:20 straight forward to build fat do Where apply the operational transformation on the Cent and do you can have

## Reference Implementation

36:32 so I already mention some of the things that Missing But as Dan mentioned Let me just
36:38 reiterate that the prototype a whole most importantly is not meant as a
36:44 Reference implementation beginning of the Reference implementation It's
36:49 eh We do not plan to to build this thing up in in the fed one directory to to add
36:56 all the things that you want for a rich system keep this as a simple
37:04 illustrative example that shows the works executable
37:10 specification we all for anyone building a comprehensive wave
37:17 server today and we be happ to supp in
37:22 Way But you know go ahead if you w put in
37:29 the BS and whistles very much appreciate patches to to fix box and to eh to track Changes to
37:40 the eh eh to the Protocol specification eh so Yes it's

## Things that didn't, let me mention

38:07 eh so some things I didn't Let me just mention These Last three points so the Access Control is very crude in this
38:14 system and it has been aed for I wouldn't be surprised if we been to la
38:22 in a place two hav for security So don't
38:30 looking n things about eh apart from any box in there there's
38:40 no support for Richer concepts of Access Control there's no groups support in
38:47 particular so we do want to support federated groups and we working on
38:52 Designs in that area but that's still work in progress and
38:58 We don't have code for and there Nothing in here eh There are no
39:05 attachments so we very much want attachments and we again same Story we
39:11 working on developing specification for how we to federate attachments and we
39:17 would want to add that once we figured that one out eh and then it's was saying that we did
39:26 implement all the measures to pring
39:34 toate Wave servers in the Protocol the way implemented
39:41 here hases signatures but we did not so if
39:48 you read the white paper that we we Put out on the Protocol in
39:54 May There are Designs in there to make signing cheaper so that you can
40:01 sign number of deltas in one shot and we still think that's
40:08 interesting Avenue but we still working out the details of SP that thing out and we
40:16 decided it was both for Clarity and because we not quite finished with
40:24 specification how this work we not included in this prototype You Will he
40:30 security session that in separate code tree there code implements how to sign
40:37 eh deltas and bundles But we we haven't quite figured out How to best put them
40:43 Together so we excluded in this round There's a lot of stuff that I
40:49 didn't cover and that will be covered in These sessions in the afternoon that Dan talked about so eh I look forward
40:59 to talking more about all these things and talking to you in during the sessions and in in all the other
41:08 avenues for conversations the day Thank
41:18 you so next on the agenda BES the feedback pizza has arrived I think Sense
41:26 grab p Quick break and then we grab
