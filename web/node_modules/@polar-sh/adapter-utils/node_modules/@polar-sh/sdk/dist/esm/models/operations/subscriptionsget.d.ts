import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type SubscriptionsGetRequest = {
    /**
     * The subscription ID.
     */
    id: string;
};
/** @internal */
export declare const SubscriptionsGetRequest$inboundSchema: z.ZodType<SubscriptionsGetRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type SubscriptionsGetRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const SubscriptionsGetRequest$outboundSchema: z.ZodType<SubscriptionsGetRequest$Outbound, z.ZodTypeDef, SubscriptionsGetRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace SubscriptionsGetRequest$ {
    /** @deprecated use `SubscriptionsGetRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<SubscriptionsGetRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `SubscriptionsGetRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<SubscriptionsGetRequest$Outbound, z.ZodTypeDef, SubscriptionsGetRequest>;
    /** @deprecated use `SubscriptionsGetRequest$Outbound` instead. */
    type Outbound = SubscriptionsGetRequest$Outbound;
}
export declare function subscriptionsGetRequestToJSON(subscriptionsGetRequest: SubscriptionsGetRequest): string;
export declare function subscriptionsGetRequestFromJSON(jsonString: string): SafeParseResult<SubscriptionsGetRequest, SDKValidationError>;
//# sourceMappingURL=subscriptionsget.d.ts.map