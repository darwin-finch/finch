import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { SubscriptionCancel, SubscriptionCancel$Outbound } from "./subscriptioncancel.js";
import { SubscriptionUpdateProduct, SubscriptionUpdateProduct$Outbound } from "./subscriptionupdateproduct.js";
export type SubscriptionUpdate = SubscriptionUpdateProduct | SubscriptionCancel;
/** @internal */
export declare const SubscriptionUpdate$inboundSchema: z.ZodType<SubscriptionUpdate, z.ZodTypeDef, unknown>;
/** @internal */
export type SubscriptionUpdate$Outbound = SubscriptionUpdateProduct$Outbound | SubscriptionCancel$Outbound;
/** @internal */
export declare const SubscriptionUpdate$outboundSchema: z.ZodType<SubscriptionUpdate$Outbound, z.ZodTypeDef, SubscriptionUpdate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace SubscriptionUpdate$ {
    /** @deprecated use `SubscriptionUpdate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<SubscriptionUpdate, z.ZodTypeDef, unknown>;
    /** @deprecated use `SubscriptionUpdate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<SubscriptionUpdate$Outbound, z.ZodTypeDef, SubscriptionUpdate>;
    /** @deprecated use `SubscriptionUpdate$Outbound` instead. */
    type Outbound = SubscriptionUpdate$Outbound;
}
export declare function subscriptionUpdateToJSON(subscriptionUpdate: SubscriptionUpdate): string;
export declare function subscriptionUpdateFromJSON(jsonString: string): SafeParseResult<SubscriptionUpdate, SDKValidationError>;
//# sourceMappingURL=subscriptionupdate.d.ts.map