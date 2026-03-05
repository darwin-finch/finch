import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { CheckoutLinkCreateProduct, CheckoutLinkCreateProduct$Outbound } from "../components/checkoutlinkcreateproduct.js";
import { CheckoutLinkCreateProductPrice, CheckoutLinkCreateProductPrice$Outbound } from "../components/checkoutlinkcreateproductprice.js";
import { CheckoutLinkCreateProducts, CheckoutLinkCreateProducts$Outbound } from "../components/checkoutlinkcreateproducts.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CheckoutLinksCreateCheckoutLinkCreate = CheckoutLinkCreateProductPrice | CheckoutLinkCreateProduct | CheckoutLinkCreateProducts;
/** @internal */
export declare const CheckoutLinksCreateCheckoutLinkCreate$inboundSchema: z.ZodType<CheckoutLinksCreateCheckoutLinkCreate, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutLinksCreateCheckoutLinkCreate$Outbound = CheckoutLinkCreateProductPrice$Outbound | CheckoutLinkCreateProduct$Outbound | CheckoutLinkCreateProducts$Outbound;
/** @internal */
export declare const CheckoutLinksCreateCheckoutLinkCreate$outboundSchema: z.ZodType<CheckoutLinksCreateCheckoutLinkCreate$Outbound, z.ZodTypeDef, CheckoutLinksCreateCheckoutLinkCreate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutLinksCreateCheckoutLinkCreate$ {
    /** @deprecated use `CheckoutLinksCreateCheckoutLinkCreate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutLinksCreateCheckoutLinkCreate, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutLinksCreateCheckoutLinkCreate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutLinksCreateCheckoutLinkCreate$Outbound, z.ZodTypeDef, CheckoutLinksCreateCheckoutLinkCreate>;
    /** @deprecated use `CheckoutLinksCreateCheckoutLinkCreate$Outbound` instead. */
    type Outbound = CheckoutLinksCreateCheckoutLinkCreate$Outbound;
}
export declare function checkoutLinksCreateCheckoutLinkCreateToJSON(checkoutLinksCreateCheckoutLinkCreate: CheckoutLinksCreateCheckoutLinkCreate): string;
export declare function checkoutLinksCreateCheckoutLinkCreateFromJSON(jsonString: string): SafeParseResult<CheckoutLinksCreateCheckoutLinkCreate, SDKValidationError>;
//# sourceMappingURL=checkoutlinkscreate.d.ts.map