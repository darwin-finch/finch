import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Product, Product$Outbound } from "./product.js";
/**
 * Sent when a product is updated.
 *
 * @remarks
 *
 * **Discord & Slack support:** Basic
 */
export type WebhookProductUpdatedPayload = {
    type?: "product.updated" | undefined;
    /**
     * A product.
     */
    data: Product;
};
/** @internal */
export declare const WebhookProductUpdatedPayload$inboundSchema: z.ZodType<WebhookProductUpdatedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookProductUpdatedPayload$Outbound = {
    type: "product.updated";
    data: Product$Outbound;
};
/** @internal */
export declare const WebhookProductUpdatedPayload$outboundSchema: z.ZodType<WebhookProductUpdatedPayload$Outbound, z.ZodTypeDef, WebhookProductUpdatedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookProductUpdatedPayload$ {
    /** @deprecated use `WebhookProductUpdatedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookProductUpdatedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookProductUpdatedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookProductUpdatedPayload$Outbound, z.ZodTypeDef, WebhookProductUpdatedPayload>;
    /** @deprecated use `WebhookProductUpdatedPayload$Outbound` instead. */
    type Outbound = WebhookProductUpdatedPayload$Outbound;
}
export declare function webhookProductUpdatedPayloadToJSON(webhookProductUpdatedPayload: WebhookProductUpdatedPayload): string;
export declare function webhookProductUpdatedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookProductUpdatedPayload, SDKValidationError>;
//# sourceMappingURL=webhookproductupdatedpayload.d.ts.map