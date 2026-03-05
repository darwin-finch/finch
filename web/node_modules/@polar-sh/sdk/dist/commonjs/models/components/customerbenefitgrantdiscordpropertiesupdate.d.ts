import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerBenefitGrantDiscordPropertiesUpdate = {
    accountId: string;
};
/** @internal */
export declare const CustomerBenefitGrantDiscordPropertiesUpdate$inboundSchema: z.ZodType<CustomerBenefitGrantDiscordPropertiesUpdate, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerBenefitGrantDiscordPropertiesUpdate$Outbound = {
    account_id: string;
};
/** @internal */
export declare const CustomerBenefitGrantDiscordPropertiesUpdate$outboundSchema: z.ZodType<CustomerBenefitGrantDiscordPropertiesUpdate$Outbound, z.ZodTypeDef, CustomerBenefitGrantDiscordPropertiesUpdate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerBenefitGrantDiscordPropertiesUpdate$ {
    /** @deprecated use `CustomerBenefitGrantDiscordPropertiesUpdate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerBenefitGrantDiscordPropertiesUpdate, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerBenefitGrantDiscordPropertiesUpdate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerBenefitGrantDiscordPropertiesUpdate$Outbound, z.ZodTypeDef, CustomerBenefitGrantDiscordPropertiesUpdate>;
    /** @deprecated use `CustomerBenefitGrantDiscordPropertiesUpdate$Outbound` instead. */
    type Outbound = CustomerBenefitGrantDiscordPropertiesUpdate$Outbound;
}
export declare function customerBenefitGrantDiscordPropertiesUpdateToJSON(customerBenefitGrantDiscordPropertiesUpdate: CustomerBenefitGrantDiscordPropertiesUpdate): string;
export declare function customerBenefitGrantDiscordPropertiesUpdateFromJSON(jsonString: string): SafeParseResult<CustomerBenefitGrantDiscordPropertiesUpdate, SDKValidationError>;
//# sourceMappingURL=customerbenefitgrantdiscordpropertiesupdate.d.ts.map