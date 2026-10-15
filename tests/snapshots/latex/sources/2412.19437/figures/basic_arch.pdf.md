Transformer Block ×

𝐿𝐿

Feed-Forward Network

RMSNorm

Attention

RMSNorm

0

Output Hidden 𝐡𝐡

𝑡𝑡 ... ...

1

...

′

𝑠𝑠

1

2

Router

... 1 ...

Multi-Head Latent Attention (MLA)

0

Output Hidden

{[ 𝐪𝐪 𝑡𝑡 , 𝑖𝑖 𝐶𝐶 ; 𝐪𝐪 𝑡𝑡 , 𝑖𝑖 𝑅𝑅 ]}

... ...

Multi-Head Attention 𝐤𝐤

𝑅𝑅

concatenate apply

{ 𝐪𝐪 𝑡𝑡 , 𝑖𝑖 𝐶𝐶 } { 𝐪𝐪 𝑡𝑡 , 𝑖𝑖 𝑅𝑅 }

{ 𝐯𝐯

{[ 𝐤𝐤 𝑡𝑡 , 𝑖𝑖 𝐶𝐶 ; 𝐤𝐤 𝑡𝑡 𝑅𝑅 ]}

𝐮𝐮

concatenate

...

Latent

Input Hidden apply 𝑡𝑡

RoPE

... ...

𝑡𝑡 , 𝑖𝑖 𝐶𝐶 } { 𝐤𝐤 𝑡𝑡 , 𝑖𝑖 𝐶𝐶 }

Latent

...

𝑡𝑡

𝑄𝑄 RoPE

𝐜𝐜

𝑡𝑡

𝑁𝑁

3

4

Routed Expert

Shared Expert

...

Top- Input Hidden

𝐾𝐾 𝑟𝑟

𝑁𝑁 𝑟𝑟 -1 𝑁𝑁

𝑟𝑟

𝐮𝐮

𝑡𝑡

Cached During Inference 𝐜𝐜

𝑡𝑡 𝐾𝐾𝐾𝐾
